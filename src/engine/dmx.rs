use std::sync::Arc;

use super::led;

use slog_scope::{error, info};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, Notify},
    time::{self, Duration, Instant},
};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

// Spec: https://www.erwinrol.com/page/articles/dmx512/

const MAX_CHANNELS: usize = 4;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub selected: bool,
}

pub fn available_ports(current_device: Option<&str>) -> anyhow::Result<Vec<DeviceInfo>> {
    let mut info: Vec<DeviceInfo> = vec![];

    let ports = tokio_serial::available_ports()?;
    for port in ports {
        if port.port_type == tokio_serial::SerialPortType::PciPort {
            continue;
        }
        info.push(DeviceInfo {
            selected: current_device.is_some_and(|n| n == port.port_name),
            name: port.port_name,
        })
    }
    Ok(info)
}

#[derive(Clone, Copy)]
struct Packet {
    data: [u8; MAX_CHANNELS],
}

impl Packet {
    fn new(red: u8, green: u8, blue: u8) -> Self {
        let mut data = [0; MAX_CHANNELS];
        data[0] = red;
        data[1] = green;
        data[2] = blue;

        Self { data }
    }
}

async fn send_dmx_packet(port: &mut SerialStream, packet: Packet) -> anyhow::Result<()> {
    let start = Instant::now();

    port.set_break()?;
    time::sleep(time::Duration::new(0, 136_000)).await;
    port.clear_break()?;

    let mut data = [0; MAX_CHANNELS + 1]; // Add space for one more start byte
    data[1..].copy_from_slice(&packet.data);
    port.write(&data).await?;

    // The max speed that DMX can update at is about 44Hz which
    // equates to 22.7 ms, (but, since we send less than 512
    // channels per-packet we can go quicker if needed)
    time::sleep(Duration::from_micros(22_700).saturating_sub(start.elapsed())).await;

    Ok(())
}

#[derive(Clone)]
pub struct SerialAgent {
    clr_tx: mpsc::Sender<led::Colour>,
    close: Arc<Notify>,
}

impl SerialAgent {
    pub fn open(port: &str) -> anyhow::Result<Self> {
        let mut port = tokio_serial::new(port, 250_000)
            .data_bits(tokio_serial::DataBits::Eight)
            .stop_bits(tokio_serial::StopBits::Two)
            .parity(tokio_serial::Parity::None)
            .flow_control(tokio_serial::FlowControl::None)
            .open_native_async()?;

        // We pipe received colours until
        // the sending channel is closed
        let (clr_tx, mut clr_rx) = mpsc::channel::<led::Colour>(32);
        let close = Arc::new(Notify::new());
        let close2 = close.clone();
        tokio::spawn(async move {
            let mut pkt = Packet::new(0, 0, 0);

            use mpsc::error::TryRecvError;
            loop {
                match clr_rx.try_recv() {
                    Ok(clr) => {
                        let srgb = clr.srgb();
                        pkt = Packet::new(srgb.red, srgb.green, srgb.blue);
                    }
                    Err(TryRecvError::Empty) => {
                        // We just wait until the next colour is received,
                        // on the channel, in the meantime we make sure to
                        // continue sending DMX updates, otherwise LEDs
                        // will turn off
                    }
                    Err(TryRecvError::Disconnected) => {
                        info!("Colour channel disconnected, exiting...");
                        close2.notify_one();
                        return;
                    }
                }

                if let Err(e) = send_dmx_packet(&mut port, pkt).await {
                    error!("Failed to send DMX packet"; "error" => format!("{:?}", e))
                }
            }
        });

        Ok(Self { clr_tx, close })
    }

    pub async fn stop(self) {
        drop(self.clr_tx);
        self.close.notified().await;
    }

    pub async fn set_colour(&self, colour: led::Colour) -> anyhow::Result<()> {
        self.clr_tx.send(colour).await?;
        Ok(())
    }

    pub fn blocking_set_colour(&self, colour: led::Colour) -> anyhow::Result<()> {
        self.clr_tx.blocking_send(colour)?;
        Ok(())
    }
}
