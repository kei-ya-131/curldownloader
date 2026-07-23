use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

pub fn split_ranges(total: u64, requested: u8) -> Vec<(u64, u64)> {
    if total == 0 {
        return Vec::new();
    }
    let count = u64::from(requested.max(1)).min(total);
    let base = total / count;
    let extra = total % count;
    let mut start = 0;
    (0..count)
        .map(|index| {
            let length = base + u64::from(index < extra);
            let range = (start, start + length - 1);
            start += length;
            range
        })
        .collect()
}

pub fn resume_offset(start: u64, end: u64, existing: u64) -> io::Result<Option<(u64, u64)>> {
    let length = end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "分段範圍無效"))?;
    if existing > length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "分段部分檔超出範圍",
        ));
    }
    if existing == length {
        return Ok(None);
    }
    let adjusted = start
        .checked_add(existing)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "分段續傳偏移無效"))?;
    Ok(Some((adjusted, end)))
}

#[derive(Default)]
pub struct ProgressMeter {
    samples: VecDeque<(u64, u64)>,
    first_ms: Option<u64>,
    paused_at: Option<u64>,
    active_ms: u64,
    last_wall_ms: Option<u64>,
    latest_bytes: u64,
}

impl ProgressMeter {
    pub fn sample(&mut self, now_ms: u64, bytes: u64) {
        if let Some(previous) = self.last_wall_ms {
            if self.paused_at.is_none() {
                self.active_ms = self
                    .active_ms
                    .saturating_add(now_ms.saturating_sub(previous));
            }
        }
        self.last_wall_ms = Some(now_ms);
        self.first_ms.get_or_insert(self.active_ms);
        self.latest_bytes = bytes;
        self.samples.push_back((self.active_ms, bytes));
        while self
            .samples
            .front()
            .is_some_and(|(timestamp, _)| self.active_ms.saturating_sub(*timestamp) > 2_000)
        {
            self.samples.pop_front();
        }
    }

    pub fn pause(&mut self, now_ms: u64) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now_ms);
        }
    }

    pub fn resume(&mut self, now_ms: u64) {
        if self.paused_at.take().is_some() {
            self.last_wall_ms = Some(now_ms);
        }
    }

    pub fn current_bps(&self) -> f64 {
        match (self.samples.front(), self.samples.back()) {
            (Some((first_time, first_bytes)), Some((last_time, last_bytes)))
                if last_time > first_time =>
            {
                last_bytes.saturating_sub(*first_bytes) as f64 * 1000.0
                    / (last_time - first_time) as f64
            }
            _ => 0.0,
        }
    }

    pub fn average_bps(&self) -> f64 {
        let Some(first) = self.first_ms else {
            return 0.0;
        };
        let active = self.active_ms.saturating_sub(first);
        if active == 0 {
            0.0
        } else {
            self.latest_bytes as f64 * 1000.0 / active as f64
        }
    }

    pub fn eta_seconds(&self, total: Option<u64>) -> Option<u64> {
        let speed = self.current_bps();
        let remaining = total?.saturating_sub(self.latest_bytes);
        (speed >= 1.0).then(|| (remaining as f64 / speed).ceil() as u64)
    }
}

pub fn validate_segment(path: &Path, start: u64, end: u64) -> io::Result<()> {
    let expected = end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "分段範圍無效"))?;
    let actual = fs::metadata(path)?.len();
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("分段大小錯誤：預期 {expected}，實際 {actual}"),
        ))
    }
}

pub fn merge_segments(parts: &[PathBuf], output: &Path, expected: u64) -> io::Result<()> {
    let mut writer = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?,
    );
    for part in parts {
        io::copy(&mut BufReader::new(File::open(part)?), &mut writer)?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    if fs::metadata(output)?.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "整合後檔案大小錯誤",
        ));
    }
    Ok(())
}

pub fn finalize_file(merged: &Path, target: &Path) -> io::Result<()> {
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "目標檔案已存在",
        ));
    }
    fs::rename(merged, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_file_once_even_when_small() {
        assert_eq!(split_ranges(3, 8), vec![(0, 0), (1, 1), (2, 2)]);
        let ranges = split_ranges(10, 4);
        assert_eq!(ranges, vec![(0, 2), (3, 5), (6, 7), (8, 9)]);
    }

    #[test]
    fn speed_excludes_paused_time() {
        let mut meter = ProgressMeter::default();
        meter.sample(0, 0);
        meter.sample(2_000, 2_000);
        meter.pause(2_000);
        meter.resume(12_000);
        meter.sample(14_000, 4_000);
        assert_eq!(meter.average_bps(), 1000.0);
        assert_eq!(meter.current_bps(), 1000.0);
    }

    #[test]
    fn resume_reports_completion_and_rejects_oversize_part() {
        assert_eq!(resume_offset(100, 199, 100).unwrap(), None);
        assert!(resume_offset(100, 199, 101).is_err());
    }

    #[test]
    fn merge_preserves_segment_order_and_finalization_rejects_collision() {
        let dir = test_dir("merge");
        let first = dir.join("segment-0.part");
        let second = dir.join("segment-1.part");
        let merged = dir.join("merged.part");
        let target = dir.join("file.bin");
        std::fs::write(&first, b"abc").unwrap();
        std::fs::write(&second, b"def").unwrap();
        merge_segments(&[second.clone(), first.clone()], &merged, 6).unwrap();
        assert_eq!(std::fs::read(&merged).unwrap(), b"defabc");
        std::fs::write(&target, b"existing").unwrap();
        assert!(finalize_file(&merged, &target).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zero_speed_and_unknown_size_have_no_eta() {
        let meter = ProgressMeter::default();
        assert_eq!(meter.current_bps(), 0.0);
        assert_eq!(meter.eta_seconds(None), None);
    }

    fn test_dir(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("curl-downloader-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
