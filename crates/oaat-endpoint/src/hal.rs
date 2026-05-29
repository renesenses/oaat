use oaat_core::format::AudioFormat;

pub trait OaatHal: Send + Sync {
    fn configure_output(&mut self, format: AudioFormat, sample_rate: u32, channels: u8)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn write_frames(&mut self, data: &[u8], frames: usize)
        -> Result<usize, Box<dyn std::error::Error + Send + Sync>>;

    fn buffer_level(&self) -> usize;

    fn set_volume(&mut self, level: u8)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn actual_sample_rate(&self) -> Option<f64>;
}
