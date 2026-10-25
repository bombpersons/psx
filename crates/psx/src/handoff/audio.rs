//! Sound for the handoff window: the SPU's mix (44.1 kHz stereo) on the
//! default output device. The game is paced by the video field rate, not by
//! the sound card, so the queue between them stays short: past
//! [`MAX_QUEUE`] the oldest samples go, and once it runs dry it plays
//! silence until [`PRIME`] has built up again.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// The SPU's output rate.
const RATE: u32 = 44_100;
/// Most stereo frames queued (about 120 ms).
const MAX_QUEUE: usize = RATE as usize * 120 / 1000;
/// Frames queued before playing starts again after running dry (about 40 ms).
const PRIME: usize = RATE as usize * 40 / 1000;

#[derive(Default)]
struct Queue {
    frames: VecDeque<[i16; 2]>,
    playing: bool,
}

pub struct Audio {
    queue: Arc<Mutex<Queue>>,
    _stream: cpal::Stream,
    /// The device's name and rate, for the panel.
    pub name: String,
}

impl Audio {
    pub fn open() -> Result<Audio, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no output device")?;
        let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "output".into());
        let default = device.default_output_config().map_err(|e| e.to_string())?;
        // The SPU's rate when the device takes it, else the device's own (resampled).
        let config = device
            .supported_output_configs()
            .ok()
            .and_then(|mut cs| {
                cs.find(|c| c.sample_format() == default.sample_format() && c.channels() == default.channels())
                    .and_then(|c| c.try_with_sample_rate(RATE))
            })
            .unwrap_or(default);
        let queue = Arc::new(Mutex::new(Queue::default()));
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config.config(), queue.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config.config(), queue.clone()),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config.config(), queue.clone()),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config.config(), queue.clone()),
            f => return Err(format!("unsupported sample format {f}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Audio { queue, _stream: stream, name: format!("{name}, {} Hz", config.sample_rate()) })
    }

    /// Queue interleaved L, R samples.
    pub fn push(&self, samples: &[i16]) {
        let mut q = self.queue.lock().unwrap();
        q.frames.extend(samples.as_chunks::<2>().0);
        let over = q.frames.len().saturating_sub(MAX_QUEUE);
        q.frames.drain(..over);
    }

    /// Drop what is queued (after a jump in time: a quick load, starting over).
    pub fn clear(&self) {
        let mut q = self.queue.lock().unwrap();
        q.frames.clear();
        q.playing = false;
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<Mutex<Queue>>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels as usize;
    // Source frames per device frame, and linear interpolation between the
    // two source frames around the playing position.
    let step = RATE as f64 / config.sample_rate as f64;
    let (mut pos, mut a, mut b) = (0.0f64, [0.0f32; 2], [0.0f32; 2]);
    device.build_output_stream(
        config,
        move |out: &mut [T], _| {
            let mut q = queue.lock().unwrap();
            if !q.playing && q.frames.len() >= PRIME {
                q.playing = true;
            }
            for frame in out.chunks_mut(channels) {
                while pos >= 1.0 {
                    pos -= 1.0;
                    a = b;
                    b = match if q.playing { q.frames.pop_front() } else { None } {
                        Some([l, r]) => [l as f32 / 32768.0, r as f32 / 32768.0],
                        None => {
                            q.playing = false;
                            [0.0; 2]
                        }
                    };
                }
                let t = pos as f32;
                let (l, r) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
                pos += step;
                match frame {
                    [one] => *one = T::from_sample((l + r) * 0.5),
                    [fl, fr, rest @ ..] => {
                        *fl = T::from_sample(l);
                        *fr = T::from_sample(r);
                        rest.iter_mut().for_each(|s| *s = T::from_sample(0.0));
                    }
                    [] => {}
                }
            }
        },
        |e| eprintln!("handoff audio: {e}"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays half a second of a 440 Hz tone on the default device:
    /// cargo test -p psx audio_plays -- --ignored
    #[test]
    #[ignore]
    fn audio_plays_a_tone() {
        let a = Audio::open().unwrap();
        eprintln!("{}", a.name);
        let tone: Vec<i16> = (0..RATE / 2)
            .flat_map(|i| {
                let v = ((i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 6000.0) as i16;
                [v, v]
            })
            .collect();
        // In field-sized pieces, as the window pushes them.
        for piece in tone.chunks(1470) {
            a.push(piece);
            std::thread::sleep(std::time::Duration::from_micros(16_667));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
