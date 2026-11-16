use super::*;

#[test]
fn dimensions_round_preserve_thin_images_and_never_enlarge() {
    for (setting, source, expected) in [
        (ExportResize::Full, (53, 11), (53, 11)),
        (ExportResize::LongEdge(20), (53, 11), (20, 4)),
        (ExportResize::LongEdge(20), (11, 53), (4, 20)),
        (ExportResize::LongEdge(100), (53, 11), (53, 11)),
        (ExportResize::LongEdge(1), (1, 10000), (1, 1)),
        (ExportResize::Percentage(50), (53, 11), (27, 6)),
        (ExportResize::Percentage(1), (1, 10000), (1, 100)),
        (ExportResize::Percentage(100), (53, 11), (53, 11)),
        (
            ExportResize::LongEdge(u32::MAX - 1),
            (u32::MAX, 1),
            (u32::MAX - 1, 1),
        ),
    ] {
        assert_eq!(setting.dimensions(source.0, source.1).unwrap(), expected);
    }
    for setting in [
        ExportResize::LongEdge(0),
        ExportResize::Percentage(0),
        ExportResize::Percentage(101),
    ] {
        assert!(setting.dimensions(10, 10).is_err());
    }
    assert!(ExportResize::Full.dimensions(0, 1).is_err());
}

// Independent direct 2D area integration, with no row cache or band iteration.
fn reference(pixels: &[Pixel], source: (u32, u32), target: (u32, u32)) -> Vec<Pixel> {
    let sx = f64::from(source.0) / f64::from(target.0);
    let sy = f64::from(source.1) / f64::from(target.1);
    let mut output = Vec::new();
    for y in 0..target.1 {
        for x in 0..target.0 {
            let x0 = f64::from(x) * sx;
            let x1 = f64::from(x + 1) * sx;
            let y0 = f64::from(y) * sy;
            let y1 = f64::from(y + 1) * sy;
            let mut sum = [0.0; 4];
            for iy in y0.floor() as u32..(y1.ceil() as u32).min(source.1) {
                for ix in x0.floor() as u32..(x1.ceil() as u32).min(source.0) {
                    let weight = (x1.min(f64::from(ix + 1)) - x0.max(f64::from(ix)))
                        * (y1.min(f64::from(iy + 1)) - y0.max(f64::from(iy)));
                    let p = pixels[(iy * source.0 + ix) as usize].map(f16::to_f64);
                    for c in 0..3 {
                        sum[c] += p[c] * p[3] * weight;
                    }
                    sum[3] += p[3] * weight;
                }
            }
            output.push([
                f16::from_f64(if sum[3] > 0.0 { sum[0] / sum[3] } else { 0.0 }),
                f16::from_f64(if sum[3] > 0.0 { sum[1] / sum[3] } else { 0.0 }),
                f16::from_f64(if sum[3] > 0.0 { sum[2] / sum[3] } else { 0.0 }),
                f16::from_f64(sum[3] / (sx * sy)),
            ]);
        }
    }
    output
}

#[test]
fn area_reduction_matches_reference_and_is_band_independent() {
    for (source, target) in [
        ((53, 11), (27, 6)),
        ((7, 5), (6, 4)),
        ((1009, 89), (13, 7)),
        ((1, 101), (1, 3)),
        ((101, 1), (3, 1)),
        ((3, 17), (3, 2)),
        ((17, 3), (2, 3)),
    ] {
        let pixels: Vec<Pixel> = (0..source.0 * source.1)
            .map(|i| {
                [
                    ((i * 37 % 101) as f32 - 20.0) / 32.0,
                    (i % 31) as f32 / 32.0,
                    0.25,
                    (i % 5) as f32 / 4.0,
                ]
                .map(f16::from_f32)
            })
            .collect();
        let expected = reference(&pixels, source, target);
        let mut previous = None;
        for band_rows in [1, 3, 17, source.1] {
            let cancel = AtomicBool::new(false);
            let bands = pixels
                .chunks(source.0 as usize * band_rows as usize)
                .map(|p| Ok(p.to_vec()));
            let resize = ResizeBands::new(bands, source, target, &cancel).unwrap();
            assert_eq!(resize.horizontal.len(), target.0 as usize);
            assert_eq!(resize.sum.len(), target.0 as usize);
            let actual: Vec<_> = resize
                .collect::<Result<Vec<_>>>()
                .unwrap()
                .into_iter()
                .flatten()
                .collect();
            assert_eq!(actual.len(), expected.len());
            for (a, e) in actual.iter().zip(&expected) {
                for c in 0..4 {
                    assert!(
                        (a[c].to_f32() - e[c].to_f32()).abs()
                            <= 0.001 * (1.0 + e[c].to_f32().abs()),
                        "{a:?} vs {e:?}"
                    );
                }
            }
            if let Some(previous) = previous {
                assert_eq!(actual, previous);
            }
            previous = Some(actual);
        }
    }
}

#[test]
fn reduction_is_linear_and_alpha_weighted_with_defined_nonfinite_handling() {
    let cancel = AtomicBool::new(false);
    for (pixels, expected) in [
        (vec![[1.0; 4], [0.0, 0.0, 0.0, 1.0]], [0.5, 0.5, 0.5, 1.0]),
        (vec![[1.0; 4], [0.0; 4]], [1.0, 1.0, 1.0, 0.5]),
        (
            vec![[1.0; 4], [f32::NAN, f32::INFINITY, 999.0, 0.0]],
            [1.0, 1.0, 1.0, 0.5],
        ),
        (
            vec![[1.0; 4], [1.0, 1.0, 1.0, f32::NAN]],
            [1.0, 1.0, 1.0, 0.5],
        ),
        (
            vec![[f32::NAN, f32::INFINITY, -1.0, 1.0], [0.0; 4]],
            [0.0, 0.0, -1.0, 0.5],
        ),
        (
            vec![[123.0, 23.0, 2.0, 0.0], [1.0, 2.0, 3.0, 0.0]],
            [0.0; 4],
        ),
    ] {
        let bands = [Ok(pixels
            .into_iter()
            .map(|p| p.map(f16::from_f32))
            .collect())]
        .into_iter();
        let actual: Vec<_> = ResizeBands::new(bands, (2, 1), (1, 1), &cancel)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(actual[0][0].map(f16::to_f32), expected);
    }
}

#[test]
fn empty_bands_fail_immediately_before_between_and_after_source_rows() {
    use std::cell::Cell;

    let cancel = AtomicBool::new(false);
    // Keep yielding empties after the selected position, then panic if the
    // resizer asks for another one. This catches a refill or finalization spin
    // without hanging the test.
    for nonempty_bands in 0..=2 {
        let calls = Cell::new(0);
        let bands = std::iter::from_fn(|| {
            let index = calls.get();
            calls.set(index + 1);
            assert!(
                index <= nonempty_bands,
                "requested another band after an empty band"
            );
            Some(Ok(if index < nonempty_bands {
                vec![[f16::ONE; 4]; 4]
            } else {
                Vec::new()
            }))
        });
        let mut resize = ResizeBands::new(bands, (2, 4), (1, 2), &cancel).unwrap();
        for _ in 0..nonempty_bands {
            assert!(resize.next().unwrap().is_ok());
        }
        let error = resize.next().unwrap().unwrap_err();
        assert!(error.to_string().contains(if nonempty_bands == 2 {
            "Unexpected resize source band"
        } else {
            "Invalid resize source band length"
        }));
        assert_eq!(calls.get(), nonempty_bands + 1);
        assert!(resize.next().is_none());
        assert_eq!(
            calls.get(),
            nonempty_bands + 1,
            "a failed stream must stop consuming bands"
        );
    }
}

#[test]
fn invalid_streams_limits_producer_errors_and_cancellation_fail() {
    let cancel = AtomicBool::new(false);
    for bands in [
        vec![],
        vec![Ok(Vec::new()), Ok(vec![[f16::ONE; 4]; 8])],
        vec![
            Ok(vec![[f16::ONE; 4]; 4]),
            Ok(Vec::new()),
            Ok(vec![[f16::ONE; 4]; 4]),
        ],
        vec![Ok(vec![[f16::ONE; 4]; 8]), Ok(Vec::new())],
        vec![Ok(vec![[f16::ONE; 4]; 3])],
        vec![Ok(vec![[f16::ONE; 4]; 16])],
        vec![Ok(vec![[f16::ONE; 4]; 4])],
        vec![Err(anyhow::anyhow!("producer failed"))],
        vec![
            Ok(vec![[f16::ONE; 4]; 8]),
            Err(anyhow::anyhow!("late producer failure")),
        ],
        vec![Ok(vec![[f16::ONE; 4]; 8]), Ok(vec![[f16::ONE; 4]; 4])],
    ] {
        let mut resize = ResizeBands::new(bands.into_iter(), (4, 2), (2, 1), &cancel).unwrap();
        assert!(resize.by_ref().collect::<Result<Vec<_>>>().is_err());
        assert!(resize.next().is_none(), "errors terminate the stream");
    }
    for target in [(0, 1), (1_000_001, 1), (1, 3), (1_000_000, 1)] {
        assert!(ResizeBands::new(std::iter::empty(), (1_000_000, 2), target, &cancel).is_err());
    }
    let bands = [Ok(vec![[f16::ONE; 4]; 8])].into_iter();
    let mut resize = ResizeBands::new(bands, (2, 4), (1, 2), &cancel).unwrap();
    assert!(resize.next().unwrap().is_ok());
    cancel.store(true, Ordering::Relaxed);
    assert!(resize.next().unwrap().is_err());
    assert!(resize.next().is_none());
    assert!(ResizeBands::new(std::iter::empty(), (4, 2), (2, 1), &cancel).is_err());
}
