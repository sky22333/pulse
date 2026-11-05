use crate::{analysis::Analysis, capture::Recording};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};

const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SAMPLES: usize = 1_000_000;
#[derive(Serialize)]
struct Output<'a> {
    format: &'static str,
    time_source: &'static str,
    recording: &'a Recording,
    analysis: &'a Analysis,
}
#[derive(Deserialize)]
struct Input {
    format: String,
    recording: Recording,
}

pub fn save(path: &Path, recording: &Recording, analysis: &Analysis) -> Result<(), String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    let data = Output {
        format: "pulse-recording-v1",
        time_source: "QPC at WM_INPUT handling; not a USB hardware timestamp",
        recording,
        analysis,
    };
    serde_json::to_writer(&mut writer, &data).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}
pub fn load(path: &Path) -> Result<Recording, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE_BYTES {
        return Err("文件超过 256 MB 的导入限制".into());
    }
    let saved: Input =
        serde_json::from_reader(BufReader::new(file)).map_err(|e| format!("文件格式无效：{e}"))?;
    if saved.format != "pulse-recording-v1" {
        return Err("不支持的录制文件格式".into());
    }
    let r = saved.recording;
    if r.frequency <= 0
        || r.samples.len() > MAX_SAMPLES
        || r.samples.iter().any(|s| s.ticks < 0)
        || r.samples.windows(2).any(|w| w[1].ticks < w[0].ticks)
    {
        return Err("录制时间戳或样本数量无效".into());
    }
    Ok(r)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        analysis::analyze,
        capture::{Kind, Sample},
    };
    #[test]
    fn recording_roundtrip() {
        let path = std::env::temp_dir().join(format!("pulse-test-{}.json", std::process::id()));
        let r = Recording {
            kind: Kind::Keyboard,
            frequency: 1_000_000,
            device: 0,
            samples: vec![Sample {
                ticks: 100,
                code: 2,
                flags: 0,
                x: 0,
                y: 0,
            }],
            dropped: 0,
            errors: 0,
        };
        save(&path, &r, &analyze(&r)).unwrap();
        let restored = load(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(restored.samples[0].ticks, 100);
        assert_eq!(restored.samples[0].code, 2);
    }
}
