use super::*;
#[test]
fn plan_covers_edges_and_obeys_limits() {
    let limits = wgpu::Limits {
        max_texture_dimension_2d: 40,
        max_buffer_size: 3 * 512,
        ..Default::default()
    };
    let plan = TilePlan::new(83, 7, &limits, 1024).unwrap();
    let mut visits = vec![0; 83 * 7];
    for tile in plan.tiles() {
        assert!(tile.width <= 40 && tile.height <= 40);
        assert!(padded_stride(tile.width) * u64::from(tile.height) <= limits.max_buffer_size);
        for y in tile.y..tile.y + tile.height {
            for x in tile.x..tile.x + tile.width {
                visits[(y * 83 + x) as usize] += 1;
            }
        }
    }
    assert!(visits.iter().all(|n| *n == 1));
    assert_eq!(plan.tile_count(), plan.tiles().count() as u64);
    assert_eq!(padded_stride(3), 256);
    assert!(TilePlan::new(1, 1, &limits, 0).is_err());
    assert!(TilePlan::new(0, 1, &limits, 32).is_err());
    assert!(
        TilePlan::new(
            1,
            1,
            &wgpu::Limits {
                max_buffer_size: 255,
                ..limits
            },
            32
        )
        .is_err()
    );
}
#[test]
fn large_plan_bounds_staging_and_band_memory() {
    let limits = wgpu::Limits::default();
    let plan = TilePlan::new(6101, 6001, &limits, 1024).unwrap();
    assert!(6101_u64 * 6001 * 8 > limits.max_buffer_size);
    for tile in plan.tiles() {
        assert!(padded_stride(tile.width) * u64::from(tile.height) <= limits.max_buffer_size);
        assert!(6101 * u64::from(tile.height) * 8 <= BAND_BUDGET);
    }
}
