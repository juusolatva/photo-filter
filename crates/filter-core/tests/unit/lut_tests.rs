use super::*;
use std::io::{Read, Write};

fn text(n: usize, f: impl Fn(usize, usize, usize) -> [f32; 3]) -> String {
    let mut out = format!("LUT_3D_SIZE {n}\n");
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let [x, y, z] = f(r, g, b);
                out.push_str(&format!("{x} {y} {z}\n"));
            }
        }
    }
    out
}
fn identity_text(n: usize) -> String {
    let s = (n - 1) as f32;
    text(n, |r, g, b| [r as f32 / s, g as f32 / s, b as f32 / s])
}
fn identity(n: usize) -> CubeLut {
    parse_cube(identity_text(n).as_bytes()).unwrap()
}
fn error(input: &str) -> String {
    parse_cube(input.as_bytes()).unwrap_err().to_string()
}
/// Deterministic values in [0, 1], f16-rounded by the parser.
fn noise(seed: usize) -> [f32; 3] {
    let mut x = (seed as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    std::array::from_fn(|_| {
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        (x >> 40) as f32 / (1u64 << 24) as f32
    })
}
fn noisy(n: usize) -> CubeLut {
    parse_cube(text(n, |r, g, b| noise(r + n * (g + n * b))).as_bytes()).unwrap()
}
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[test]
fn parses_headers_titles_domains_and_keyword_case() {
    let lut = parse_cube(identity_text(2).as_bytes()).unwrap();
    assert_eq!((lut.size(), lut.title()), (2, ""));
    assert_eq!((lut.domain_min(), lut.domain_max()), ([0.0; 3], [1.0; 3]));
    let body = identity_text(2);
    let quoted = format!("# note\nTitle \"Warm # look\"\n{body}");
    assert_eq!(parse_cube(quoted.as_bytes()).unwrap().title(), "Warm");
    let quoted = format!("TITLE \"Warm Film\"\n{body}");
    assert_eq!(parse_cube(quoted.as_bytes()).unwrap().title(), "Warm Film");
    let bare = format!("title   Plain Name  \n{body}");
    assert_eq!(parse_cube(bare.as_bytes()).unwrap().title(), "Plain Name");
    let domain = format!("domain_min -0.5\t0 0\nDOMAIN_MAX 2  1.5 1\n{body}");
    let lut = parse_cube(domain.as_bytes()).unwrap();
    assert_eq!(
        (lut.domain_min(), lut.domain_max()),
        ([-0.5, 0.0, 0.0], [2.0, 1.5, 1.0])
    );
    let range = format!("LUT_3D_INPUT_RANGE 0.0 4\n{body}");
    let lut = parse_cube(range.as_bytes()).unwrap();
    assert_eq!((lut.domain_min(), lut.domain_max()), ([0.0; 3], [4.0; 3]));
}

#[test]
fn input_range_conflicts_with_explicit_domains_and_repeats() {
    let body = identity_text(2);
    for header in [
        "DOMAIN_MIN 0 0 0\nLUT_3D_INPUT_RANGE 0 1\n",
        "LUT_3D_INPUT_RANGE 0 1\nDOMAIN_MAX 1 1 1\n",
        "LUT_3D_INPUT_RANGE 0 1\nDOMAIN_MIN 0 0 0\n",
    ] {
        let message = error(&format!("{header}{body}"));
        assert!(
            message.contains("line 2") && message.contains("INPUT_RANGE"),
            "{message}"
        );
    }
    let message = error(&format!(
        "LUT_3D_INPUT_RANGE 0 1\nLUT_3D_INPUT_RANGE 0 1\n{body}"
    ));
    assert!(message.contains("line 2: duplicate"), "{message}");
}

#[test]
fn line_endings_bom_comments_and_blank_lines() {
    let lf = identity_text(2);
    let reference = parse_cube(lf.as_bytes()).unwrap();
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(parse_cube(crlf.as_bytes()).unwrap(), reference);
    let mixed = lf.replacen('\n', "\r\n", 3);
    assert_eq!(parse_cube(mixed.as_bytes()).unwrap(), reference);
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(lf.as_bytes());
    assert_eq!(parse_cube(&bom).unwrap(), reference);
    let commented = format!("# header\n\n   \n{}", lf.replace('\n', " # trailing\n\n"));
    assert_eq!(parse_cube(commented.as_bytes()).unwrap(), reference);
    assert_eq!(parse_cube(lf.trim_end().as_bytes()).unwrap(), reference);
}

#[test]
fn rejects_lone_carriage_returns_and_control_characters() {
    let mac = identity_text(2).replace('\n', "\r");
    assert!(error(&mac).contains("line 1: line endings not supported"));
    let mid = format!("LUT_3D_SIZE 2\n0 0\r0\n{}", "0 0 0\n".repeat(8));
    assert!(error(&mid).contains("line 2: line endings"));
    assert!(error("LUT_3D_SIZE 2\u{0}\n").contains("line 1: unexpected control"));
    assert!(error("LUT_3D_SIZE 2\n0 0 \u{7} 0\n").contains("line 2: unexpected control"));
    // Comments are ignored, whatever they contain.
    let commented = format!("# \u{0}\u{7}\n{}", identity_text(2));
    assert!(parse_cube(commented.as_bytes()).is_ok());
}

#[test]
fn titles_are_sanitized_and_latin1_degrades() {
    let body = identity_text(2);
    let tabbed = format!("TITLE \"a\tb\"\n{body}");
    assert_eq!(parse_cube(tabbed.as_bytes()).unwrap().title(), "ab");
    let controlled = format!("TITLE \"a\u{7}b\"\n{body}");
    assert!(error(&controlled).contains("line 1: unexpected control"));
    let long = format!("TITLE \"{}\"\n{body}", "x".repeat(400));
    assert_eq!(
        parse_cube(long.as_bytes()).unwrap().title().chars().count(),
        128
    );
    let mut latin1 = b"TITLE \"Caf\xE9\"\n".to_vec();
    latin1.extend_from_slice(body.as_bytes());
    assert_eq!(parse_cube(&latin1).unwrap().title(), "Caf\u{FFFD}");
}

#[test]
fn rejects_unsupported_and_misplaced_keywords() {
    let body = identity_text(2);
    assert!(error(&format!("LUT_1D_SIZE 4\n{body}")).contains("1D and combined"));
    assert!(error(&format!("{body}LUT_1D_SIZE 4\n")).contains("1D and combined"));
    for name in [
        "LUT_1D_INPUT_RANGE",
        "LUT_IN_VIDEO_RANGE",
        "LUT_OUT_VIDEO_RANGE",
        "FOO",
    ] {
        let message = error(&format!("{name} 0 1\n{body}"));
        assert_eq!(message, format!("unsupported keyword {name} on line 1"));
    }
    let long = "K".repeat(100);
    let message = error(&format!("{long} 1\n{body}"));
    assert_eq!(
        message,
        format!("unsupported keyword {} on line 1", "K".repeat(32))
    );
    assert!(error("TITLE a\nTITLE b\nLUT_3D_SIZE 2\n").contains("line 2: duplicate keyword TITLE"));
    assert!(error(&format!("{body}\nLUT_3D_SIZE 2\n")).contains("line 11: header keyword"));
    assert!(error(&format!("{body}TITLE late\n")).contains("after data"));
    assert!(
        error("DOMAIN_MIN 0 0\nLUT_3D_SIZE 2\n").contains("line 1: DOMAIN_MIN needs exactly 3")
    );
    assert!(error("DOMAIN_MIN 0 0 0\nDOMAIN_MIN 0 0 0\n").contains("line 2: duplicate"));
}

#[test]
fn rejects_missing_duplicate_and_bad_sizes() {
    assert_eq!(error(""), "missing LUT_3D_SIZE before the data");
    assert_eq!(error("0 0 0\n"), "missing LUT_3D_SIZE before the data");
    assert!(error("LUT_3D_SIZE 2\nLUT_3D_SIZE 2\n").contains("line 2: duplicate"));
    for size in [
        "0",
        "1",
        "66",
        "1000",
        "-2",
        "two",
        "2.0",
        "",
        "99999999999999999999",
        "0x10",
    ] {
        let message = error(&format!("LUT_3D_SIZE {size}\n"));
        assert!(
            message.contains("line 1: LUT_3D_SIZE must be"),
            "{size}: {message}"
        );
    }
    assert!(error("LUT_3D_SIZE 2 3\n").contains("line 1: LUT_3D_SIZE must be"));
}

#[test]
fn rejects_short_long_and_malformed_data() {
    let body = identity_text(2);
    let short = body.lines().take(8).collect::<Vec<_>>().join("\n");
    assert_eq!(error(&short), "expected 8 data rows, found 7");
    assert_eq!(error("LUT_3D_SIZE 2\n"), "expected 8 data rows, found 0");
    let long = format!("{body}0 0 0\n");
    assert_eq!(error(&long), "extra data after line 9");
    let two = body.replacen("1 0 0", "1 0", 1);
    assert!(error(&two).contains("line 3: data row needs exactly 3 values"));
    let four = body.replacen("1 0 0", "1 0 0 0", 1);
    assert!(error(&four).contains("line 3: data row needs exactly 3 values"));
    for token in [
        "nan", "NaN", "inf", "-inf", "infinity", "0x10", "1e999", "--1", "1,5", "abc",
    ] {
        let bad = body.replacen("1 0 0", &format!("{token} 0 0"), 1);
        let message = error(&bad);
        assert!(message.contains("line 3"), "{token}: {message}");
    }
    let big = body.replacen("1 0 0", "8.5 0 0", 1);
    assert!(error(&big).contains("value out of range on line 3"));
    let negative = body.replacen("1 0 0", "-8.5 0 0", 1);
    assert!(error(&negative).contains("value out of range on line 3"));
    let edge = body.replacen("1 0 0", "8 -8 1e1", 1);
    assert!(error(&edge).contains("value out of range on line 3"));
    let ok = body.replacen("1 0 0", "8 -8 +.5", 1);
    assert!(parse_cube(ok.as_bytes()).is_ok());
}

#[test]
fn rejects_bad_domains_with_the_keyword_line() {
    let body = identity_text(2);
    for domain in [
        "DOMAIN_MAX 0 1 1",
        "DOMAIN_MAX 1 1 -1",
        "DOMAIN_MIN 1 0 0",
        "DOMAIN_MAX 0.0000001 1 1",
    ] {
        let message = error(&format!("# c\n{domain}\n{body}"));
        assert!(
            message.contains("line 2: domain must be"),
            "{domain}: {message}"
        );
    }
    assert!(error(&format!("DOMAIN_MAX 9 1 1\n{body}")).contains("value out of range on line 1"));
    assert!(error(&format!("DOMAIN_MIN nan 0 0\n{body}")).contains("line 1: invalid number"));
    assert!(
        error("LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 0 1 1\n").contains("line 3: domain")
    );
}

#[test]
fn rejects_long_lines_and_oversized_files() {
    let body = identity_text(2);
    let message = error(&format!("# {}\n{body}", "x".repeat(1100)));
    assert!(message.contains("line 1: line is longer than 1024"));
    assert!(parse_cube(format!("# {}\n{body}", "x".repeat(1000)).as_bytes()).is_ok());
    let big = vec![b'#'; MAX_CUBE_BYTES + 1];
    assert!(parse_cube(&big).unwrap_err().to_string().contains("24 MiB"));
}

#[test]
fn data_order_is_red_fastest() {
    let n = 4;
    let lut = parse_cube(text(n, |r, g, b| [r as f32, g as f32, b as f32]).as_bytes()).unwrap();
    for (r, g, b) in [(1, 0, 0), (0, 2, 0), (0, 0, 3), (3, 1, 2), (2, 3, 1)] {
        assert_eq!(lut.node([r, g, b]), [r as f32, g as f32, b as f32]);
        let index = 3 * (r + n * (g + n * b));
        assert_eq!(lut.texels()[index], f16::from_f32(r as f32).to_bits());
    }
}

#[test]
fn values_are_stored_as_f16_rounded_to_nearest_even() {
    let body = identity_text(2).replacen("1 0 0", "0.1 0.30000001 1.0004", 1);
    let lut = parse_cube(body.as_bytes()).unwrap();
    let [r, g, b] = lut.node([1, 0, 0]);
    assert_eq!(r, f16::from_f32(0.1).to_f32());
    assert_eq!(g, f16::from_f32(0.3).to_f32());
    assert_eq!(b, 1.0);
    assert!(r != 0.1 && (r - 0.1).abs() < 5e-5);
}

#[test]
fn parser_never_panics_on_truncation_or_mutation() {
    let valid = format!("TITLE \"t\"\nDOMAIN_MAX 1 1 1\n{}", identity_text(3));
    let bytes = valid.as_bytes();
    for end in 0..=bytes.len() {
        let _ = parse_cube(&bytes[..end]);
    }
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..10_000 {
        let mut copy = bytes.to_vec();
        for _ in 0..=(rng.next() % 4) {
            let at = (rng.next() as usize) % copy.len();
            copy[at] = rng.next() as u8;
        }
        let _ = parse_cube(&copy);
    }
}

#[test]
fn lattice_points_are_exact() {
    let lut = noisy(5);
    for b in 0..5 {
        for g in 0..5 {
            for r in 0..5 {
                let t = [r as f32 / 4.0, g as f32 / 4.0, b as f32 / 4.0];
                assert_eq!(lut.tetra(t), lut.node([r, g, b]), "{r} {g} {b}");
            }
        }
    }
}

#[test]
fn identity_luts_reproduce_their_input() {
    let two = identity(2);
    let mut rng = Rng(7);
    for _ in 0..200 {
        let t = [rng.unit(), rng.unit(), rng.unit()];
        let out = two.tetra(t);
        assert!(
            (0..3).all(|c| (out[c] - t[c]).abs() < 1e-6),
            "{t:?} {out:?}"
        );
    }
    for n in [17, 33, 65] {
        let lut = identity(n);
        for _ in 0..200 {
            let t = [rng.unit(), rng.unit(), rng.unit()];
            let out = lut.tetra(t);
            assert!(
                (0..3).all(|c| (out[c] - t[c]).abs() < 5e-4),
                "{n} {t:?} {out:?}"
            );
        }
    }
}

#[test]
fn gray_axis_follows_the_diagonal_lerp() {
    let lut = noisy(9);
    for k in 0..=100 {
        let g = k as f32 / 100.0;
        let out = lut.tetra([g; 3]);
        let p = g * 8.0;
        let i = (p as usize).min(7);
        let f = p - i as f32;
        let (c0, c1) = (lut.node([i; 3]), lut.node([i + 1; 3]));
        for c in 0..3 {
            assert!((out[c] - (c0[c] + f * (c1[c] - c0[c]))).abs() < 1e-6, "{g}");
        }
    }
}

fn barycentric(lut: &CubeLut, t: [f32; 3]) -> [f64; 3] {
    let nm1 = (lut.size() - 1) as f64;
    let p = t.map(|v| f64::from(v) * nm1);
    let top = lut.size() - 2;
    let i: [usize; 3] = p.map(|v| (v as usize).min(top));
    let f: [f64; 3] = std::array::from_fn(|k| p[k] - i[k] as f64);
    let order = tetra_order(f.map(|v| v as f32));
    let mut corner = i;
    let mut weights = vec![(1.0 - f[order[0]], i)];
    for (step, axis) in order.iter().enumerate() {
        corner[*axis] += 1;
        let next = order.get(step + 1).map_or(0.0, |a| f[*a]);
        weights.push((f[*axis] - next, corner));
    }
    let mut out = [0.0; 3];
    for (w, c) in weights {
        for (k, v) in lut.node(c).iter().enumerate() {
            out[k] += w * f64::from(*v);
        }
    }
    out
}

#[test]
fn all_six_tetrahedra_match_a_barycentric_evaluation() {
    let lut = noisy(7);
    let mut rng = Rng(99);
    let mut seen = std::collections::HashSet::new();
    let mut points: Vec<[f32; 3]> = (0..3000)
        .map(|_| [rng.unit(), rng.unit(), rng.unit()])
        .collect();
    for _ in 0..500 {
        // Near and exactly on faces and the diagonal: ties and one-ulp neighbours.
        let (a, b) = (rng.unit(), rng.unit());
        points.push([a, a, b]);
        points.push([a, b, a]);
        points.push([b, a, a]);
        points.push([a, a, a]);
        points.push([a, a + 1e-6, a - 1e-6]);
    }
    for t in points {
        let t = t.map(|v| v.clamp(0.0, 1.0));
        let f = t.map(|v| (v * 6.0).fract());
        seen.insert(tetra_order(f));
        let (got, want) = (lut.tetra(t), barycentric(&lut, t));
        for c in 0..3 {
            assert!((f64::from(got[c]) - want[c]).abs() < 1e-5, "{t:?}");
        }
    }
    assert_eq!(seen.len(), 6, "{seen:?}");
}

#[test]
fn tie_breaking_matches_the_shader_order() {
    assert_eq!(tetra_order([0.5, 0.5, 0.5]), [0, 1, 2]);
    assert_eq!(tetra_order([0.5, 0.5, 0.7]), [2, 0, 1]);
    assert_eq!(tetra_order([0.5, 0.3, 0.5]), [0, 2, 1]);
    assert_eq!(tetra_order([0.3, 0.5, 0.5]), [1, 2, 0]);
    assert_eq!(tetra_order([0.3, 0.5, 0.3]), [1, 0, 2]);
    assert_eq!(tetra_order([0.1, 0.2, 0.3]), [2, 1, 0]);
}

#[test]
fn interpolation_is_continuous_across_cell_faces() {
    let lut = noisy(6);
    let mut rng = Rng(5);
    let delta = 1e-5_f32;
    for _ in 0..500 {
        let base = [rng.unit(), rng.unit(), rng.unit()];
        for axis in 0..3 {
            let k = (rng.next() % 4 + 1) as f32 / 5.0;
            let (mut lo, mut hi) = (base, base);
            lo[axis] = k - delta;
            hi[axis] = k + delta;
            let (a, b) = (lut.tetra(lo), lut.tetra(hi));
            assert!(
                (0..3).all(|c| (a[c] - b[c]).abs() < 1e-3),
                "{base:?} {axis}"
            );
        }
    }
}

#[test]
fn hash_equality_and_lattice_comparison() {
    let a = noisy(4);
    let b = noisy(4);
    assert_eq!(a.hash(), b.hash());
    assert_eq!(a, b);
    let titled = parse_cube(
        format!("TITLE x\n{}", text(4, |r, g, b| noise(r + 4 * (g + 4 * b)))).as_bytes(),
    )
    .unwrap();
    assert!(a.same_lattice(&titled));
    assert_ne!(a, titled);
    let other = identity(4);
    assert!(!a.same_lattice(&other));
    assert_ne!(a.hash(), other.hash());
    let shifted = format!(
        "DOMAIN_MAX 2 2 2\n{}",
        text(4, |r, g, b| noise(r + 4 * (g + 4 * b)))
    );
    assert!(!a.same_lattice(&parse_cube(shifted.as_bytes()).unwrap()));
    assert!(format!("{a:?}").len() < 100);
}

#[test]
fn new_validates_every_field() {
    let ok = vec![0_u16; 3 * 8];
    assert!(CubeLut::new(2, "", [0.0; 3], [1.0; 3], ok.clone()).is_ok());
    assert_eq!(
        CubeLut::new(1, "", [0.0; 3], [1.0; 3], ok.clone()).unwrap_err(),
        CubeError::Size(1)
    );
    assert!(CubeLut::new(2, "", [0.0; 3], [1.0; 3], vec![0; 5]).is_err());
    assert!(CubeLut::new(2, "", [0.0; 3], [0.0; 3], ok.clone()).is_err());
    assert!(CubeLut::new(2, "", [f32::NAN; 3], [1.0; 3], ok.clone()).is_err());
    for bits in [0x7C00_u16, 0xFC00, 0x7E00, 0x4880, 0xC880] {
        let mut bad = ok.clone();
        bad[5] = bits;
        assert_eq!(
            CubeLut::new(2, "", [0.0; 3], [1.0; 3], bad).unwrap_err(),
            CubeError::TexelValue
        );
    }
}

#[test]
fn slots_compare_intern_and_validate() {
    let a = LutSlot::Cube(Arc::new(noisy(3)));
    let same_content = LutSlot::Cube(Arc::new(noisy(3)));
    assert_eq!(a, a.clone());
    assert_eq!(a, same_content);
    assert_ne!(a, LutSlot::None);
    assert_eq!(LutSlot::None, LutSlot::default());
    let titled = LutSlot::Cube(Arc::new(
        parse_cube(
            format!("TITLE x\n{}", text(3, |r, g, b| noise(r + 3 * (g + 3 * b)))).as_bytes(),
        )
        .unwrap(),
    ));
    assert_ne!(a, titled);
    let interned = same_content.clone().interned(&a);
    assert!(Arc::ptr_eq(interned.cube().unwrap(), a.cube().unwrap()));
    let kept = titled.clone().interned(&a);
    assert!(Arc::ptr_eq(kept.cube().unwrap(), titled.cube().unwrap()));
    assert!(LutSlot::None.interned(&a) == LutSlot::None);
    a.validate().unwrap();
    LutSlot::None.validate().unwrap();
    assert_eq!(LutSlot::None.estimated_bytes(), 0);
    assert!(a.estimated_bytes() >= 3 * 27 * 2);
}

fn base64_of(bytes: &[u8]) -> String {
    base64::encode(bytes)
}
fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    encoder.write_all(raw).unwrap();
    encoder.finish().unwrap()
}
fn embed(raw: &[u8]) -> String {
    base64_of(&zlib(raw))
}
fn decode_as(size: usize, data: &str) -> Result<CubeLut, CubeError> {
    CubeLut::from_embedded("t", size, [0.0; 3], [1.0; 3], EMBED_ENCODING, data)
}
fn message(result: Result<CubeLut, CubeError>) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn base64_matches_the_rfc_4648_vectors() {
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64::encode(plain.as_bytes()), encoded);
        assert_eq!(base64::decode(encoded).unwrap(), plain.as_bytes());
    }
    let all: Vec<u8> = (0..=255).collect();
    assert_eq!(base64::decode(&base64::encode(&all)).unwrap(), all);
    assert_eq!(base64::encode(&[0xfb, 0xff]), "+/8=");
}

#[test]
fn base64_rejects_malformed_input() {
    for bad in [
        "Zg=",
        "Zg",
        "Zg= =",
        "Zh==",
        "Zm9=",
        "Zg==Zg==",
        "Z-9v",
        "Z_9v",
        "Zm 9v",
        "Zm9v\n",
        "====",
        "Zm9v=",
        "=Zg=",
        "Z===",
        "Zm9\u{e9}",
        " Zm9v",
        "Zm9v ",
    ] {
        assert!(base64::decode(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn embedded_round_trip_restores_the_exact_lattice() {
    for n in [2, 17] {
        let lut = noisy(n);
        let data = lut.to_embedded();
        assert!(data.len().is_multiple_of(4) && data.is_ascii());
        let back = decode_as(n, &data).unwrap();
        assert_eq!(back.texels(), lut.texels());
        assert_eq!(back.hash(), lut.hash());
    }
    let domain = CubeLut::from_embedded(
        "x",
        2,
        [-1.0; 3],
        [2.0; 3],
        EMBED_ENCODING,
        &identity(2).to_embedded(),
    )
    .unwrap();
    assert_eq!(
        (domain.domain_min(), domain.domain_max(), domain.title()),
        ([-1.0; 3], [2.0; 3], "x")
    );
}

#[test]
fn embedded_headers_are_checked_before_the_data() {
    let junk = "!!!!";
    for size in [0, 1, 66, 1000] {
        assert_eq!(
            message(decode_as(size, junk)),
            CubeError::Size(size).to_string()
        );
    }
    let bad_domain = CubeLut::from_embedded("", 2, [0.0; 3], [0.0; 3], EMBED_ENCODING, junk);
    assert!(message(bad_domain).contains("domain"));
    let bad_encoding = CubeLut::from_embedded("", 2, [0.0; 3], [1.0; 3], "f16-zlib", junk);
    assert_eq!(message(bad_encoding), "unsupported LUT encoding");
    assert_eq!(message(decode_as(2, junk)), "LUT data is not valid base64");
}

#[test]
fn embedded_data_over_the_size_cap_is_rejected_unread() {
    let raw_len = 17 * 17 * 17 * 6;
    let limit = base64_max(raw_len);
    assert_eq!(limit % 4, 0);
    let at_cap = "A".repeat(limit);
    assert_ne!(
        message(decode_as(17, &at_cap)),
        "LUT data exceeds the size limit"
    );
    let over = "A".repeat(limit + 4);
    assert_eq!(
        message(decode_as(17, &over)),
        "LUT data exceeds the size limit"
    );
    assert!(base64_max(65 * 65 * 65 * 6) < 3 << 20);
}

#[test]
fn a_compression_bomb_cannot_inflate_past_the_declared_size() {
    let zeros = vec![0_u8; 10 << 20];
    let data = embed(&zeros);
    assert!(data.len() <= base64_max(17 * 17 * 17 * 6), "{}", data.len());
    assert_eq!(
        message(decode_as(17, &data)),
        "LUT data is larger than declared"
    );

    struct Endless(usize);
    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            buf.fill(0);
            self.0 += buf.len();
            Ok(buf.len())
        }
    }
    let mut source = Endless(0);
    assert!(read_exact_bounded(&mut source, 100).is_err());
    assert!(source.0 <= 101, "{}", source.0);
}

#[test]
fn embedded_streams_must_match_exactly() {
    let raw_len = 17 * 17 * 17 * 6;
    let lut = noisy(17);
    let compressed = zlib(&shuffle(lut.texels()));
    let truncated = base64_of(&compressed[..compressed.len() / 2]);
    assert_eq!(message(decode_as(17, &truncated)), "LUT data is truncated");
    let short = embed(&vec![0_u8; raw_len - 2]);
    assert_eq!(message(decode_as(17, &short)), "LUT data is truncated");
    let mut corrupted = compressed.clone();
    *corrupted.last_mut().unwrap() ^= 0x55;
    assert_eq!(
        message(decode_as(17, &base64_of(&corrupted))),
        "LUT data is corrupt"
    );
    let mut trailing = compressed.clone();
    trailing.extend_from_slice(&[0, 1, 2, 3]);
    assert_eq!(
        message(decode_as(17, &base64_of(&trailing))),
        "LUT data has trailing bytes"
    );
    assert_eq!(
        message(decode_as(65, &lut.to_embedded())),
        "LUT data is truncated"
    );
    assert!(decode_as(17, &base64_of(&compressed)).is_ok());
    assert_eq!(message(decode_as(17, "")), "LUT data is truncated");
}

#[test]
fn embedded_values_must_be_finite_and_bounded() {
    for bits in [0x7C00_u16, 0x7E00, 0xFC00, 0x4880] {
        let mut texels = identity(2).texels().to_vec();
        texels[7] = bits;
        let data = embed(&shuffle(&texels));
        assert_eq!(
            message(decode_as(2, &data)),
            CubeError::TexelValue.to_string()
        );
    }
}

#[test]
fn slots_serialize_as_objects_and_reject_unknown_content() {
    let none = serde_json::to_value(LutSlot::None).unwrap();
    assert_eq!(none, serde_json::json!({"kind":"none"}));
    assert_eq!(
        serde_json::from_value::<LutSlot>(none).unwrap(),
        LutSlot::None
    );
    let slot = LutSlot::Cube(Arc::new(noisy(3)));
    let value = serde_json::to_value(&slot).unwrap();
    assert_eq!(value["kind"], "cube");
    assert_eq!(value["encoding"], EMBED_ENCODING);
    assert!(value.get("hash").is_none());
    assert_eq!(
        serde_json::from_value::<LutSlot>(value.clone()).unwrap(),
        slot
    );
    for edit in [
        |v: &mut serde_json::Value| v["extra"] = 1.into(),
        |v: &mut serde_json::Value| v["kind"] = "other".into(),
        |v: &mut serde_json::Value| v["encoding"] = "raw".into(),
        |v: &mut serde_json::Value| v["size"] = 4.into(),
        |v: &mut serde_json::Value| {
            v.as_object_mut().unwrap().remove("data");
        },
    ] {
        let mut broken = value.clone();
        edit(&mut broken);
        assert!(serde_json::from_value::<LutSlot>(broken).is_err());
    }
    assert!(serde_json::from_value::<LutSlot>(serde_json::json!({"kind":"none","x":1})).is_err());
    assert!(serde_json::from_value::<LutSlot>(serde_json::json!(null)).is_err());
    let mut titled = value;
    titled["title"] = "a\u{7}b".into();
    let loaded: LutSlot = serde_json::from_value(titled).unwrap();
    assert_eq!(loaded.cube().unwrap().title(), "ab");
}
