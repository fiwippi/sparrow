use std::time::Duration;

pub struct Config {
    // Powers of 2 are quickest to calculate, regardless, a buffer
    // size of 16384 gives us (almost) decimal accuracy, so we
    // don't want to configure it any bigger
    pub buffer_size: usize,

    // Set to 0 to disable
    pub min_period: Duration,
}
