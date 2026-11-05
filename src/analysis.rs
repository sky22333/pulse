use crate::capture::Recording;
use serde::Serialize;

const N: usize = 32768;
const POINTS: usize = 160;
const MAX_PAIRS: usize = 2_000_000;

#[derive(Clone, Copy, Default, Serialize)]
pub struct PlotPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Default, Serialize)]
pub struct Analysis {
    pub count: usize,
    pub seconds: f64,
    pub rate: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub coincident_gaps: usize,
    pub time_charts: Vec<Vec<PlotPoint>>,
    pub frequency_charts: Vec<Vec<PlotPoint>>,
    pub pairs_sampled: bool,
    pub time_peaks_ms: Vec<Option<f64>>,
    pub frequency_peaks_hz: Vec<Option<f64>>,
}

fn points(values: &[f64], begin: usize, end: usize, scale: f32) -> Vec<PlotPoint> {
    (0..POINTS)
        .map(|i| {
            let a = begin + (end - begin) * i / POINTS;
            let b = begin + (end - begin) * (i + 1) / POINTS;
            let index = (a..b.max(a + 1).min(values.len()))
                .max_by(|&x, &y| values[x].total_cmp(&values[y]))
                .unwrap_or(a);
            PlotPoint {
                x: index as f32 * scale,
                y: values[index] as f32,
            }
        })
        .collect()
}
fn spectrum(values: &[f64], fft: &dyn realfft::RealToComplex<f64>) -> Vec<f64> {
    let mut input = values.to_vec();
    let mut output = fft.make_output_vec();
    if fft.process(&mut input, &mut output).is_err() {
        return vec![0.; N / 2 + 1];
    }
    output.iter().map(|v| v.norm()).collect()
}
// Deterministic hash randomises positions within sampling strata so a fixed
// stride cannot lock onto a high-rate device's periodicity.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}
pub fn analyze(r: &Recording) -> Analysis {
    let mut a = Analysis {
        count: r.samples.len(),
        ..Default::default()
    };
    if r.samples.len() < 2 || r.frequency <= 0 {
        return a;
    }
    a.seconds = (r.samples.last().unwrap().ticks - r.samples[0].ticks) as f64 / r.frequency as f64;
    if a.seconds > 0. {
        a.rate = (a.count - 1) as f64 / a.seconds;
    }
    let mut gaps: Vec<f64> = r
        .samples
        .windows(2)
        .filter_map(|w| {
            let ticks = w[1].ticks - w[0].ticks;
            (ticks >= 0).then_some(ticks as f64 * 1000. / r.frequency as f64)
        })
        .collect();
    if gaps.is_empty() {
        return a;
    }
    a.coincident_gaps = gaps.iter().filter(|&&gap| gap == 0.).count();
    let mut consecutive = vec![0.; N];
    let mut pairs = vec![0.; N];
    let mut wrapped = vec![0.; N];
    let idx = |ticks: i64| ((ticks as i128 * N as i128) / r.frequency as i128) as usize;
    for w in r.samples.windows(2) {
        let delta = w[1].ticks - w[0].ticks;
        if delta >= 0 && delta < r.frequency {
            consecutive[idx(delta)] += 1.;
        }
    }
    // Count eligible pairs in O(n), then distribute any bounded sampling across
    // the entire recording instead of analysing only the beginning.
    let mut total_pairs = 0_u64;
    let mut left = 0;
    for i in 0..r.samples.len() {
        while left < i && r.samples[i].ticks - r.samples[left].ticks >= r.frequency {
            left += 1;
        }
        total_pairs = total_pairs.saturating_add((i - left) as u64);
    }
    let sampled = total_pairs > MAX_PAIRS as u64;
    a.pairs_sampled = sampled;
    let anchors = if sampled {
        r.samples.len().min(1000)
    } else {
        r.samples.len()
    };
    let per_anchor = if sampled {
        MAX_PAIRS / anchors.max(1)
    } else {
        MAX_PAIRS
    };
    left = 0;
    for anchor in 0..anchors {
        let i = if sampled {
            anchor * r.samples.len() / anchors
        } else {
            anchor
        };
        while left < i && r.samples[i].ticks - r.samples[left].ticks >= r.frequency {
            left += 1;
        }
        let available = i - left;
        let used = available.min(per_anchor);
        if used == 0 {
            continue;
        }
        // Spread selected neighbours across the full one-second window and
        // weight them so dense and sparse anchors remain comparable.
        let anchor_weight = r.samples.len() as f64 / anchors as f64;
        let weight = available as f64 / used as f64 * anchor_weight;
        for k in 0..used {
            let start = k * available / used;
            let end = (k + 1) * available / used;
            let offset = if end - start == 1 {
                0
            } else {
                (mix(((i as u64) << 32) ^ k as u64) % (end - start) as u64) as usize
            };
            let j = left + start + offset;
            let delta = r.samples[i].ticks - r.samples[j].ticks;
            pairs[idx(delta)] += weight;
        }
    }
    for sample in &r.samples {
        wrapped[idx(sample.ticks.rem_euclid(r.frequency))] += 1.;
    }
    let mut percentile = |p: f64| {
        let index = ((gaps.len() - 1) as f64 * p).round() as usize;
        *gaps.select_nth_unstable_by(index, f64::total_cmp).1
    };
    a.median_ms = percentile(0.5);
    a.p95_ms = percentile(0.95);
    a.p99_ms = percentile(0.99);
    let mut planner = realfft::RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(N);
    let spectra = [
        spectrum(&consecutive, fft.as_ref()),
        spectrum(&pairs, fft.as_ref()),
        spectrum(&wrapped, fft.as_ref()),
    ];
    let peak = |data: &[f64], begin: usize, end: usize| -> Option<usize> {
        (begin..end)
            .max_by(|&x, &y| data[x].total_cmp(&data[y]))
            .filter(|&i| data[i] > 0.)
    };
    a.time_peaks_ms = [(&consecutive, N / 50), (&pairs, N / 10), (&wrapped, N)]
        .into_iter()
        .map(|(data, end)| peak(data, 0, end).map(|i| i as f64 * 1000. / N as f64))
        .collect();
    a.frequency_peaks_hz = spectra
        .iter()
        .map(|data| peak(data, 50, 8001).map(|i| i as f64))
        .collect();
    a.time_charts = vec![
        points(&consecutive, 0, N / 50, 1000. / N as f32),
        points(&pairs, 0, N / 10, 1000. / N as f32),
        points(&wrapped, 0, N, 1000. / N as f32),
    ];
    a.frequency_charts = spectra.iter().map(|s| points(s, 50, 8001, 1.)).collect();

    a
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Kind, Sample};
    fn rec(kind: Kind, samples: Vec<Sample>) -> Recording {
        Recording {
            kind,
            frequency: 1_000_000,
            device: 1,
            samples,
            dropped: 0,
            errors: 0,
        }
    }
    fn sample(ticks: i64) -> Sample {
        Sample {
            ticks,
            code: 1,
            flags: 0,
            x: 1,
            y: 0,
        }
    }
    #[test]
    fn rate_and_intervals() {
        let r = rec(Kind::Mouse, (0..101).map(|i| sample(i * 1000)).collect());
        let a = analyze(&r);
        assert_eq!(a.rate, 1000.);
        assert_eq!(a.median_ms, 1.);
        assert_eq!(a.time_charts.len(), 3);
        assert_eq!(a.frequency_charts.len(), 3);
    }
    #[test]
    fn keyboard_can_show_8000_hz_peak() {
        let r = rec(Kind::Keyboard, (0..1600).map(|i| sample(i * 125)).collect());
        assert_eq!(analyze(&r).frequency_peaks_hz[2], Some(8000.));
    }
    #[test]
    fn empty_recording_is_safe() {
        let a = analyze(&rec(Kind::Mouse, vec![]));
        assert_eq!(a.count, 0);
        assert_eq!(a.rate, 0.);
    }
    #[test]
    fn dense_pairs_are_bounded() {
        let r = rec(Kind::Mouse, (0..4000).map(sample).collect());
        assert!(analyze(&r).pairs_sampled);
    }
    #[test]
    fn sampled_pairs_cover_late_recording() {
        let mut samples: Vec<_> = (0..4000).map(sample).collect();
        samples.extend((0..4000).map(|i| sample(5_000_000 + i * 500)));
        let a = analyze(&rec(Kind::Mouse, samples));
        assert!(a.pairs_sampled);
        assert!(a.time_charts[1].iter().skip(20).any(|v| v.y > 0.));
    }
    #[test]
    fn pair_peak_matches_known_intervals() {
        let r = rec(
            Kind::Keyboard,
            vec![sample(0), sample(1_000), sample(2_000)],
        );
        let a = analyze(&r);
        let peak = a.time_peaks_ms[1].unwrap();
        assert!((peak - 1.0).abs() < 0.04, "{peak}");
        assert!(!a.pairs_sampled);
    }
    #[test]
    fn plotted_point_retains_actual_peak_position_and_value() {
        let mut values = vec![0.; 320];
        values[101] = 7.;
        let plotted = points(&values, 0, 320, 0.5);
        assert_eq!(plotted[50].x, 50.5);
        assert_eq!(plotted[50].y, 7.);
    }
    #[test]
    fn event_rate_matches_known_frequencies_up_to_8khz() {
        for hz in [125_i64, 500, 1000, 4000, 8000] {
            let period = 1_000_000 / hz;
            let r = rec(Kind::Mouse, (0..=hz).map(|i| sample(i * period)).collect());
            let result = analyze(&r);
            assert!(
                (result.rate - hz as f64).abs() < 0.001,
                "{hz}: {}",
                result.rate
            );
            assert!((result.median_ms - 1000. / hz as f64).abs() < 0.001);
        }
    }
    #[test]
    fn pauses_are_included_in_observed_rate() {
        let mut samples: Vec<_> = (0..=100).map(|i| sample(i * 125)).collect();
        samples.extend((101..=200).map(|i| sample(i * 125 + 1_000_000)));
        let a = analyze(&rec(Kind::Mouse, samples));
        assert!((a.rate - 200. / 1.025).abs() < 0.001);
        assert_eq!(a.median_ms, 0.125);
    }
    #[test]
    fn batched_events_do_not_become_precise_usb_timestamps() {
        let samples = (0..200).map(|i| sample((i / 2) * 250)).collect();
        let a = analyze(&rec(Kind::Mouse, samples));
        assert_eq!(a.median_ms, 0.0);
        assert_eq!(a.coincident_gaps, 100);
        assert!(a.p95_ms >= 0.25);
        assert!(a.rate > 8000.); // Mean event cadence may exceed the nominal rate at boundaries.
    }
    #[test]
    fn display_bins_are_independent_not_running_maxima() {
        let mut spectrum = vec![0.; 320];
        spectrum[0] = 50.;
        spectrum[3] = 3.;
        let plotted = points(&spectrum, 0, 320, 1.);
        assert_eq!([plotted[0].y, plotted[1].y, plotted[2].y], [50., 3., 0.]);
    }
    #[test]
    fn fft_impulse_has_equal_amplitude_in_each_frequency_bin() {
        let mut planner = realfft::RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(N);
        let mut impulse = vec![0.; N];
        impulse[123] = 3.;
        let result = spectrum(&impulse, fft.as_ref());
        for i in [50, 1000, 4000, 8000] {
            assert!((result[i] - 3.).abs() < 1e-9, "bin {i}");
        }
    }
    #[test]
    fn selected_percentiles_match_sort_reference() {
        let samples = vec![
            sample(0),
            sample(300),
            sample(400),
            sample(900),
            sample(1100),
            sample(1800),
        ];
        let a = analyze(&rec(Kind::Mouse, samples));
        let mut gaps = [0.3_f64, 0.1, 0.5, 0.2, 0.7];
        gaps.sort_by(f64::total_cmp);
        let quantile = |p: f64| gaps[((gaps.len() - 1) as f64 * p).round() as usize];
        assert!((a.median_ms - quantile(0.5)).abs() < 1e-12);
        assert!((a.p95_ms - quantile(0.95)).abs() < 1e-12);
        assert!((a.p99_ms - quantile(0.99)).abs() < 1e-12);
    }
    #[test]
    fn sampled_8khz_pairs_approximate_complete_distribution() {
        let samples: Vec<_> = (0..3000).map(|i| sample(i * 125)).collect();
        let mut exact = vec![0.; N];
        for i in 0..samples.len() {
            for j in 0..i {
                let d = samples[i].ticks - samples[j].ticks;
                exact[(d as usize * N) / 1_000_000] += 1.;
            }
        }
        let expected = points(&exact, 0, N / 10, 1000. / N as f32);
        let a = analyze(&rec(Kind::Mouse, samples));
        assert!(a.pairs_sampled);
        let error: f64 = a.time_charts[1]
            .iter()
            .zip(&expected)
            .map(|(x, y)| (x.y - y.y).abs() as f64)
            .sum();
        let total: f64 = expected.iter().map(|p| p.y as f64).sum();
        assert!(error / total < 0.12, "relative error {}", error / total);
        assert_eq!(a.frequency_peaks_hz[1], Some(8000.));
    }
    #[test]
    #[ignore = "manual performance check"]
    fn stress_1m_at_8khz() {
        let r = rec(
            Kind::Mouse,
            (0..1_000_000).map(|i| sample(i * 125)).collect(),
        );
        let start = std::time::Instant::now();
        let a = analyze(&r);
        eprintln!("一百万事件分析耗时：{:?}", start.elapsed());
        assert!((a.rate - 8000.).abs() < 0.001);
        assert!(a.pairs_sampled);
    }
    #[test]
    fn jitter_and_missing_events_are_reflected_in_observed_rate() {
        let jittered: Vec<_> = (0..=8000)
            .map(|i| sample(i * 125 + if i % 2 == 0 { 0 } else { 10 }))
            .collect();
        let a = analyze(&rec(Kind::Mouse, jittered));
        assert!((a.rate - 8000.).abs() < 0.001);
        assert!(a.p95_ms > 0.125);
        let missing: Vec<_> = (0..=8000)
            .filter(|i| i % 4 != 0)
            .map(|i| sample(i * 125))
            .collect();
        let a = analyze(&rec(Kind::Mouse, missing));
        assert!((a.rate - 6000.).abs() < 2.);
    }
}
