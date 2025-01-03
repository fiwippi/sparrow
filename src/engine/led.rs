use std::{collections::HashMap, fmt, str::FromStr};

use anyhow::anyhow; // TODO Concrete errors
use palette::{oklch::Oklch, FromColor, Mix, OklabHue, Srgb};

pub const BLACK: Colour = Colour(Oklch::new_const(0.0, 0.0, OklabHue::new(0.0)));

#[derive(Debug, Clone, Copy)]
pub struct Colour(Oklch);

impl FromStr for Colour {
    type Err = anyhow::Error; // TODO Concrete error

    /// From hex, i.e. "#ffffff"
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let srgb_u8: Srgb<u8> = Srgb::from_str(s)?;
        let srgb_f32: Srgb<f32> = srgb_u8.into_format();
        Ok(Colour(Oklch::from_color(srgb_f32)))
    }
}

impl fmt::Display for Colour {
    /// To hex, i.e. "#ffffff"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let srgb: Srgb<u8> = Srgb::from_color(self.0).into_format();
        write!(f, "#{:02x}{:02x}{:02x}", srgb.red, srgb.green, srgb.blue)
    }
}

impl Mix for Colour {
    type Scalar = f32;

    fn mix(self, other: Self, factor: Self::Scalar) -> Self {
        Colour(self.0.mix(other.0, factor))
    }
}

#[derive(Debug, Clone)]
pub struct Gradient {
    /// Colours are sorted by position
    /// in the range [0.0, 1.0]
    pub colours: Vec<(Colour, f32)>,
}

impl Gradient {
    pub fn new() -> Self {
        Self { colours: vec![] }
    }

    pub fn change_colour(&mut self, index: usize, value: Colour) {
        self.colours[index].0 = value;
    }

    pub fn change_position(&mut self, index: usize, position: f32) {
        self.colours[index].1 = position;
        self.colours.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    }

    pub fn add_colour(&mut self, clr: Colour) {
        self.colours.push((clr, 1.0))
    }

    pub fn delete_colour(&mut self, index: usize) {
        self.colours.remove(index);
    }

    pub fn interpolate(&self, position: f32) -> Colour {
        if self.colours.len() == 0 {
            return BLACK;
        } else if self.colours.len() == 1 {
            return self.colours[0].0;
        }

        for i in 0..self.colours.len() - 1 {
            let left = self.colours[i];
            let right = self.colours[i + 1];
            if left.1 <= position && position <= right.1 {
                let factor = (position - left.1) / (right.1 - left.1);
                return left.0.mix(right.0, factor);
            }
        }

        // If we haven't reached the position yet, our colour
        // is either before the first point or after the last
        // one
        if position < self.colours[0].1 {
            self.colours[0].0
        } else {
            self.colours[self.colours.len() - 1].0
        }
    }

    pub fn bar(&self, points: usize) -> Vec<Colour> {
        let mut colours = Vec::<Colour>::new();

        let interval = 1.0 / points as f32;
        let mut position = 0.0;
        while position < 1.0 {
            colours.push(self.interpolate(position));
            position += interval;
        }
        colours.push(self.interpolate(1.0));

        colours
    }
}

#[derive(Debug, Clone)]
pub struct GradientInfo {
    pub name: String,
    pub selected: bool,
    pub data: Gradient,
}

pub struct Gradients {
    current: Option<String>,
    gradients: HashMap<String, Gradient>,
}

impl Gradients {
    pub fn new() -> Self {
        Self {
            current: None,
            gradients: HashMap::new(),
        }
    }

    pub fn info(&self) -> Vec<GradientInfo> {
        let mut info: Vec<GradientInfo> = vec![];

        for (name, gradient) in self.gradients.iter() {
            let name = name.clone();
            info.push(GradientInfo {
                selected: self.current.as_ref().is_some_and(|n| *n == name),
                data: gradient.clone(),
                name,
            })
        }

        info.sort_by(|a, b| a.name.cmp(&b.name));
        info
    }

    pub fn set_current(&mut self, name: &str) -> anyhow::Result<()> {
        if !self.gradients.contains_key(name) {
            return Err(anyhow!("gradient does not exist"));
        }
        self.current = Some(name.to_string());

        Ok(())
    }

    pub fn get(&self, name: &str) -> anyhow::Result<Gradient> {
        if let Some(gradient) = self.gradients.get(name) {
            Ok(gradient.clone())
        } else {
            Err(anyhow!("gradient does not exist"))
        }
    }

    pub fn add(&mut self, name: &str, gradient: Gradient, overwrite: bool) -> anyhow::Result<()> {
        if !overwrite && self.gradients.contains_key(name) {
            return Err(anyhow!("gradient already exists"));
        }

        self.gradients.insert(name.to_string(), gradient);
        self.current = Some(name.to_string());

        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> anyhow::Result<()> {
        self.gradients.remove(name);
        if self.current.as_ref().is_some_and(|n| n == name) {
            self.current = None;
        }

        Ok(())
    }
}
