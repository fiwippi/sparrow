use std::sync::Arc;

use super::led;

use slog_scope::{error, info};
use tokio::{
    io::AsyncWriteExt,
    sync::{watch, Notify},
    time::{self, Duration, Instant},
};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

// Spec: https://www.erwinrol.com/page/articles/dmx512/

const MAX_CHANNELS: usize = 3;

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
    clr_tx: watch::Sender<led::Colour>,
    shutdown_tx: watch::Sender<bool>,
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
        let (clr_tx, clr_rx) = watch::channel(led::BLACK);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let close = Arc::new(Notify::new());
        let close2 = close.clone();
        tokio::spawn(async move {
            let mut pkt;

            loop {
                if *shutdown_rx.borrow() == true {
                    info!("Colour channel disconnected, exiting...");
                    close2.notify_one();
                    return;
                }
                match *clr_rx.borrow() {
                    clr => {
                        let srgb = clr.srgb();
                        pkt = Packet::new(srgb.red, srgb.green, srgb.blue);
                    }
                }

                if let Err(e) = send_dmx_packet(&mut port, pkt).await {
                    error!("Failed to send DMX packet"; "error" => format!("{:?}", e));
                    // Once a device is disconnected, even if it's reconnected we don't
                    // have the logic to begin piping to it again (too complicated), so
                    // we just abort the main loop
                    if let Ok(e) = e.downcast::<tokio_serial::Error>() {
                        // In some cases, we may get a disconnection error which
                        // doesn't have the NoDevice kind, so we have to check
                        // the description manually
                        let a = e.kind == tokio_serial::ErrorKind::NoDevice;
                        let b = e.description == "No such device or address";
                        if a || b {
                            error!("The DMX device does not exist, aborting DMX connection");
                            // If we don't do this, the audio pipe will hang on close
                            close2.notify_one();
                            return;
                        }
                    }
                }
            }
        });

        Ok(Self {
            clr_tx,
            shutdown_tx,
            close,
        })
    }

    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        self.close.notified().await;
    }

    pub fn set_colour(&self, colour: led::Colour) -> anyhow::Result<()> {
        self.clr_tx.send(colour)?;
        Ok(())
    }
}
