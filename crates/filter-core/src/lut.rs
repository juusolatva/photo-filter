//! Cube 3D LUTs: the `.cube` parser, the f16 lattice, the tetrahedral CPU
//! reference and the preset embedding codec. Independent of I/O, GUI and GPU.
//!
//! The lattice stores encoded-sRGB output values as half floats, rounded to
//! nearest even at construction, so the CPU and the GPU interpolate the same f32
//! values. Data order is red fastest, then green, then blue.
use crate::edit::{ModelError, Validate};
use half::f16;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{
    fmt,
    io::{self, Read, Write},
    sync::Arc,
};

pub const LUT_MIN_SIZE: usize = 2;
pub const LUT_MAX_SIZE: usize = 65;
/// Lattice values and domain bounds satisfy `|v| <= MAX_LUT_VALUE`.
pub const MAX_LUT_VALUE: f32 = 8.0;
pub const MAX_DOMAIN_ABS: f32 = 8.0;
/// Smallest allowed `max - min` per channel.
pub const MIN_DOMAIN_WIDTH: f32 = 1e-6;
pub const MAX_TITLE_CHARS: usize = 128;
/// Larger `.cube` files are rejected unread; a 65^3 file is 8 to 16 MB.
pub const MAX_CUBE_BYTES: usize = 24 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 1024;
/// Preset payload encoding: little-endian f16 bytes shuffled into two planes, zlib, base64.
pub const EMBED_ENCODING: &str = "f16-shuffle-zlib-v1";

/// Why a `.cube` file or an embedded LUT was rejected. Line numbers are 1-based.
/// Messages never echo more than a truncated keyword name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CubeError {
    TooLarge,
    LineEndings {
        line: usize,
    },
    LineTooLong {
        line: usize,
    },
    ControlCharacter {
        line: usize,
    },
    Unsupported1D {
        line: usize,
    },
    UnsupportedKeyword {
        line: usize,
        name: String,
    },
    DuplicateKeyword {
        line: usize,
        name: String,
    },
    HeaderAfterData {
        line: usize,
        name: String,
    },
    RangeConflict {
        line: usize,
    },
    MissingSize,
    BadArguments {
        line: usize,
        name: String,
        count: usize,
    },
    BadSize {
        line: usize,
    },
    BadNumber {
        line: usize,
    },
    ValueOutOfRange {
        line: usize,
    },
    ShortData {
        expected: usize,
        found: usize,
    },
    ExtraData {
        line: usize,
    },
    Size(usize),
    TexelCount {
        expected: usize,
        found: usize,
    },
    TexelValue,
    Domain {
        line: Option<usize>,
    },
    Embedded(&'static str),
}
impl fmt::Display for CubeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "LUT file exceeds the {} MiB limit", MAX_CUBE_BYTES >> 20),
            Self::LineEndings { line } => {
                write!(
                    f,
                    "line {line}: line endings not supported (use LF or CRLF)"
                )
            }
            Self::LineTooLong { line } => {
                write!(f, "line {line}: line is longer than {MAX_LINE_BYTES} bytes")
            }
            Self::ControlCharacter { line } => {
                write!(f, "line {line}: unexpected control character")
            }
            Self::Unsupported1D { line } => write!(
                f,
                "line {line}: 1D and combined 1D+3D .cube files are not supported"
            ),
            Self::UnsupportedKeyword { line, name } => {
                write!(f, "unsupported keyword {name} on line {line}")
            }
            Self::DuplicateKeyword { line, name } => {
                write!(f, "line {line}: duplicate keyword {name}")
            }
            Self::HeaderAfterData { line, name } => {
                write!(f, "line {line}: header keyword {name} after data")
            }
            Self::RangeConflict { line } => write!(
                f,
                "line {line}: LUT_3D_INPUT_RANGE cannot be combined with DOMAIN_MIN or DOMAIN_MAX"
            ),
            Self::MissingSize => f.write_str("missing LUT_3D_SIZE before the data"),
            Self::BadArguments { line, name, count } => {
                write!(f, "line {line}: {name} needs exactly {count} values")
            }
            Self::BadSize { line } => write!(
                f,
                "line {line}: LUT_3D_SIZE must be an integer from {LUT_MIN_SIZE} to {LUT_MAX_SIZE}"
            ),
            Self::BadNumber { line } => write!(f, "line {line}: invalid number"),
            Self::ValueOutOfRange { line } => {
                write!(
                    f,
                    "value out of range on line {line} (limit +-{MAX_LUT_VALUE})"
                )
            }
            Self::ShortData { expected, found } => {
                write!(f, "expected {expected} data rows, found {found}")
            }
            Self::ExtraData { line } => write!(f, "extra data after line {line}"),
            Self::Size(n) => {
                write!(f, "LUT size {n} is outside {LUT_MIN_SIZE}..={LUT_MAX_SIZE}")
            }
            Self::TexelCount { expected, found } => {
                write!(f, "LUT has {found} values, expected {expected}")
            }
            Self::TexelValue => write!(f, "LUT values must be finite and within +-{MAX_LUT_VALUE}"),
            Self::Domain { line: Some(line) } => write!(
                f,
                "line {line}: domain must be finite, within +-{MAX_DOMAIN_ABS} and wider than {MIN_DOMAIN_WIDTH}"
            ),
            Self::Domain { line: None } => write!(
                f,
                "LUT domain must be finite, within +-{MAX_DOMAIN_ABS} and wider than {MIN_DOMAIN_WIDTH}"
            ),
            Self::Embedded(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for CubeError {}

/// An immutable, validated LUT. Fields are private: it is created only by
/// [`CubeLut::new`], [`parse_cube`] and [`CubeLut::from_embedded`].
pub struct CubeLut {
    size: u8,
    title: String,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    /// f16 bits, `3 * N^3` values, red fastest, then green, then blue.
    texels: Box<[u16]>,
    /// FNV-1a 64 of size, domain bit patterns and texels. A dedupe hint only.
    hash: u64,
}
impl fmt::Debug for CubeLut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CubeLut {{ title: {:?}, size: {}, hash: {:08x} }}",
            self.title,
            self.size,
            self.hash >> 32
        )
    }
}
impl PartialEq for CubeLut {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title && self.same_lattice(other)
    }
}

fn domain_is_valid(min: [f32; 3], max: [f32; 3]) -> bool {
    (0..3).all(|c| {
        min[c].abs() <= MAX_DOMAIN_ABS
            && max[c].abs() <= MAX_DOMAIN_ABS
            && max[c] - min[c] >= MIN_DOMAIN_WIDTH
    })
}
fn sanitize_title(title: &str) -> String {
    let kept: String = title
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect();
    kept.trim().to_owned()
}
fn node_count(size: usize) -> usize {
    size * size * size
}
fn fnv1a(size: u8, min: [f32; 3], max: [f32; 3], texels: &[u16]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(&[size]);
    for v in min.iter().chain(&max) {
        feed(&v.to_bits().to_le_bytes());
    }
    for t in texels {
        feed(&t.to_le_bytes());
    }
    hash
}
/// Tetrahedron traversal order: axes sorted by descending fraction, ties broken
/// x before y before z. Identical to the branch structure in `lut.wgsl`.
fn tetra_order(f: [f32; 3]) -> [usize; 3] {
    if f[0] >= f[1] {
        if f[1] >= f[2] {
            [0, 1, 2]
        } else if f[0] >= f[2] {
            [0, 2, 1]
        } else {
            [2, 0, 1]
        }
    } else if f[0] >= f[2] {
        [1, 0, 2]
    } else if f[1] >= f[2] {
        [1, 2, 0]
    } else {
        [2, 1, 0]
    }
}

impl CubeLut {
    /// Validate and build. `texels` are f16 bits in lattice order.
    pub fn new(
        size: usize,
        title: &str,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
        texels: Vec<u16>,
    ) -> Result<Self, CubeError> {
        if !(LUT_MIN_SIZE..=LUT_MAX_SIZE).contains(&size) {
            return Err(CubeError::Size(size));
        }
        if !domain_is_valid(domain_min, domain_max) {
            return Err(CubeError::Domain { line: None });
        }
        let expected = 3 * node_count(size);
        if texels.len() != expected {
            return Err(CubeError::TexelCount {
                expected,
                found: texels.len(),
            });
        }
        if texels.iter().any(|t| {
            let v = f16::from_bits(*t).to_f32();
            !v.is_finite() || v.abs() > MAX_LUT_VALUE
        }) {
            return Err(CubeError::TexelValue);
        }
        let size = size as u8;
        let hash = fnv1a(size, domain_min, domain_max, &texels);
        Ok(Self {
            size,
            title: sanitize_title(title),
            domain_min,
            domain_max,
            texels: texels.into_boxed_slice(),
            hash,
        })
    }
    /// Lattice edge length N.
    pub fn size(&self) -> usize {
        usize::from(self.size)
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn domain_min(&self) -> [f32; 3] {
        self.domain_min
    }
    pub fn domain_max(&self) -> [f32; 3] {
        self.domain_max
    }
    /// f16 bits, `3 * N^3` values, red fastest, then green, then blue.
    pub fn texels(&self) -> &[u16] {
        &self.texels
    }
    /// Dedupe hint (FNV-1a 64); never an integrity or trust value.
    pub fn hash(&self) -> u64 {
        self.hash
    }
    /// Everything except the title is identical. A hash match is always
    /// confirmed with a full texel comparison.
    pub fn same_lattice(&self, other: &Self) -> bool {
        let bits = |v: [f32; 3]| v.map(f32::to_bits);
        self.size == other.size
            && self.hash == other.hash
            && bits(self.domain_min) == bits(other.domain_min)
            && bits(self.domain_max) == bits(other.domain_max)
            && self.texels == other.texels
    }
    /// The lattice value at integer coordinates, converted exactly to f32.
    pub fn node(&self, c: [usize; 3]) -> [f32; 3] {
        let n = self.size();
        let i = 3 * (c[0] + n * (c[1] + n * c[2]));
        std::array::from_fn(|k| f16::from_bits(self.texels[i + k]).to_f32())
    }
    /// Tetrahedral interpolation at `t`, already finite and clamped to `[0, 1]^3`.
    /// The same expression order as `lut_tetra` in `lut.wgsl`; no fused multiply-add.
    pub fn tetra(&self, t: [f32; 3]) -> [f32; 3] {
        let nm1 = f32::from(self.size - 1);
        let p = t.map(|v| v * nm1);
        let top = self.size() - 2;
        let i: [usize; 3] = p.map(|v| (v as usize).min(top));
        let f: [f32; 3] = std::array::from_fn(|k| p[k] - i[k] as f32);
        let mut corner = i;
        let mut previous = self.node(corner);
        let mut v = previous;
        for axis in tetra_order(f) {
            corner[axis] += 1;
            let current = self.node(corner);
            v = std::array::from_fn(|k| v[k] + f[axis] * (current[k] - previous[k]));
            previous = current;
        }
        v
    }
}

/// Exponent-bit test, identical to `any_nonfinite3` in WGSL.
pub(crate) fn any_nonfinite3(v: [f32; 3]) -> bool {
    v.iter().any(|x| x.to_bits() & 0x7f80_0000 == 0x7f80_0000)
}

// ---------------------------------------------------------------------------
// .cube parser

/// Parse a `.cube` file. Pure: no I/O. See `docs/pointwise.md` for the grammar.
pub fn parse_cube(bytes: &[u8]) -> Result<CubeLut, CubeError> {
    if bytes.len() > MAX_CUBE_BYTES {
        return Err(CubeError::TooLarge);
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    // Meaningful lines are ASCII; stray bytes only degrade titles or fail numbers.
    let text = String::from_utf8_lossy(bytes);
    let mut parser = CubeParser::default();
    for (index, raw) in text.split('\n').enumerate() {
        parser.line(index + 1, raw)?;
    }
    parser.finish()
}

#[derive(Default)]
struct CubeParser {
    size: usize,
    title: Option<String>,
    domain_min: Option<[f32; 3]>,
    domain_max: Option<[f32; 3]>,
    input_range: bool,
    domain_line: usize,
    texels: Vec<u16>,
    rows: usize,
    last_data_line: usize,
}
fn truncated(name: &str) -> String {
    name.chars().take(32).collect()
}
fn parse_value(token: &str, line: usize) -> Result<f32, CubeError> {
    let first = token.as_bytes()[0];
    if !(first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'.')) {
        return Err(CubeError::BadNumber { line });
    }
    let value: f32 = token.parse().map_err(|_| CubeError::BadNumber { line })?;
    if value.is_finite() && value.abs() <= MAX_LUT_VALUE {
        Ok(value)
    } else {
        Err(CubeError::ValueOutOfRange { line })
    }
}
fn parse_numbers<const N: usize>(
    rest: &str,
    line: usize,
    name: &str,
) -> Result<[f32; N], CubeError> {
    let bad = || CubeError::BadArguments {
        line,
        name: truncated(name),
        count: N,
    };
    let mut tokens = rest.split_ascii_whitespace();
    let mut out = [0.0; N];
    for slot in &mut out {
        *slot = parse_value(tokens.next().ok_or_else(bad)?, line)?;
    }
    if tokens.next().is_some() {
        return Err(bad());
    }
    Ok(out)
}
fn parse_title(rest: &str) -> String {
    let text = match rest.strip_prefix('"') {
        Some(inner) => inner.rfind('"').map_or(inner, |end| &inner[..end]),
        None => rest,
    };
    sanitize_title(text)
}
impl CubeParser {
    fn line(&mut self, line: usize, raw: &str) -> Result<(), CubeError> {
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        if text.contains('\r') {
            return Err(CubeError::LineEndings { line });
        }
        if text.len() > MAX_LINE_BYTES {
            return Err(CubeError::LineTooLong { line });
        }
        let content = text.split('#').next().unwrap_or("");
        if content.chars().any(|c| c.is_control() && c != '\t') {
            return Err(CubeError::ControlCharacter { line });
        }
        let content = content.trim_matches(|c: char| c.is_ascii_whitespace());
        match content.chars().next() {
            None => Ok(()),
            Some(c) if c.is_ascii_alphabetic() || c == '_' => self.keyword(line, content),
            Some(_) => self.data_row(line, content),
        }
    }
    fn keyword(&mut self, line: usize, content: &str) -> Result<(), CubeError> {
        let (name, rest) = content
            .split_once(|c: char| c.is_ascii_whitespace())
            .map_or((content, ""), |(name, rest)| (name, rest.trim()));
        let upper = name.to_ascii_uppercase();
        let known = matches!(
            upper.as_str(),
            "TITLE" | "LUT_3D_SIZE" | "DOMAIN_MIN" | "DOMAIN_MAX" | "LUT_3D_INPUT_RANGE"
        );
        if upper == "LUT_1D_SIZE" {
            return Err(CubeError::Unsupported1D { line });
        }
        if !known {
            return Err(CubeError::UnsupportedKeyword {
                line,
                name: truncated(name),
            });
        }
        if self.rows > 0 {
            return Err(CubeError::HeaderAfterData { line, name: upper });
        }
        match upper.as_str() {
            "TITLE" => self.set_title(line, rest),
            "LUT_3D_SIZE" => self.set_size(line, rest),
            "LUT_3D_INPUT_RANGE" => self.set_input_range(line, rest),
            _ => self.set_domain(line, &upper, rest),
        }
    }
    fn duplicate(line: usize, name: &str) -> CubeError {
        CubeError::DuplicateKeyword {
            line,
            name: name.to_owned(),
        }
    }
    fn set_title(&mut self, line: usize, rest: &str) -> Result<(), CubeError> {
        if self.title.is_some() {
            return Err(Self::duplicate(line, "TITLE"));
        }
        self.title = Some(parse_title(rest));
        Ok(())
    }
    fn set_size(&mut self, line: usize, rest: &str) -> Result<(), CubeError> {
        if self.size != 0 {
            return Err(Self::duplicate(line, "LUT_3D_SIZE"));
        }
        let bad = CubeError::BadSize { line };
        if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad);
        }
        let size: usize = rest.parse().map_err(|_| bad.clone())?;
        if !(LUT_MIN_SIZE..=LUT_MAX_SIZE).contains(&size) {
            return Err(bad);
        }
        self.size = size;
        // Allocate only after the size is validated.
        self.texels = Vec::with_capacity(3 * node_count(size));
        Ok(())
    }
    fn set_input_range(&mut self, line: usize, rest: &str) -> Result<(), CubeError> {
        if self.input_range {
            return Err(Self::duplicate(line, "LUT_3D_INPUT_RANGE"));
        }
        if self.domain_min.is_some() || self.domain_max.is_some() {
            return Err(CubeError::RangeConflict { line });
        }
        let [lo, hi] = parse_numbers(rest, line, "LUT_3D_INPUT_RANGE")?;
        self.input_range = true;
        self.domain_min = Some([lo; 3]);
        self.domain_max = Some([hi; 3]);
        self.domain_line = line;
        Ok(())
    }
    fn set_domain(&mut self, line: usize, name: &str, rest: &str) -> Result<(), CubeError> {
        if self.input_range {
            return Err(CubeError::RangeConflict { line });
        }
        let slot = if name == "DOMAIN_MIN" {
            &mut self.domain_min
        } else {
            &mut self.domain_max
        };
        if slot.is_some() {
            return Err(Self::duplicate(line, name));
        }
        *slot = Some(parse_numbers(rest, line, name)?);
        self.domain_line = line;
        Ok(())
    }
    fn domain(&self) -> Result<([f32; 3], [f32; 3]), CubeError> {
        let min = self.domain_min.unwrap_or([0.0; 3]);
        let max = self.domain_max.unwrap_or([1.0; 3]);
        if domain_is_valid(min, max) {
            Ok((min, max))
        } else {
            Err(CubeError::Domain {
                line: Some(self.domain_line),
            })
        }
    }
    fn data_row(&mut self, line: usize, content: &str) -> Result<(), CubeError> {
        if self.size == 0 {
            return Err(CubeError::MissingSize);
        }
        if self.rows == 0 {
            self.domain()?;
        }
        if self.rows == node_count(self.size) {
            return Err(CubeError::ExtraData {
                line: self.last_data_line,
            });
        }
        let rgb: [f32; 3] = parse_numbers(content, line, "data row")?;
        self.texels.extend(rgb.map(|v| f16::from_f32(v).to_bits()));
        self.rows += 1;
        self.last_data_line = line;
        Ok(())
    }
    fn finish(self) -> Result<CubeLut, CubeError> {
        if self.size == 0 {
            return Err(CubeError::MissingSize);
        }
        let (min, max) = self.domain()?;
        let expected = node_count(self.size);
        if self.rows != expected {
            return Err(CubeError::ShortData {
                expected,
                found: self.rows,
            });
        }
        let title = self.title.unwrap_or_default();
        CubeLut::new(self.size, &title, min, max, self.texels)
    }
}

// ---------------------------------------------------------------------------
// Slot in the edit model

/// The LUT of the Finishing section. A non-nullable enum so that a preset
/// patch can express "clear the LUT" (`Option<Arc<_>>` would serialize `None`
/// as `null`, which reads back as "field absent").
#[derive(Clone, Debug, Default)]
pub enum LutSlot {
    #[default]
    None,
    Cube(Arc<CubeLut>),
}
impl PartialEq for LutSlot {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::Cube(a), Self::Cube(b)) => Arc::ptr_eq(a, b) || a == b,
            _ => false,
        }
    }
}
impl LutSlot {
    pub fn cube(&self) -> Option<&Arc<CubeLut>> {
        match self {
            Self::None => None,
            Self::Cube(lut) => Some(lut),
        }
    }
    /// Reuse `current`'s `Arc` when this is the same LUT (lattice and title), so
    /// an identical load or preset creates no undo step and no GPU upload.
    pub fn interned(self, current: &Self) -> Self {
        match (&self, current) {
            (Self::Cube(a), Self::Cube(b)) if a.same_lattice(b) && a.title == b.title => {
                current.clone()
            }
            _ => self,
        }
    }
    /// Conservative heap estimate; shared `Arc`s are deliberately not deduplicated.
    pub fn estimated_bytes(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Cube(lut) => {
                std::mem::size_of::<CubeLut>() + lut.texels.len() * 2 + lut.title.len()
            }
        }
    }
    /// Constant-time: the other invariants hold by construction.
    fn check(&self) -> Result<(), ModelError> {
        let Self::Cube(lut) = self else { return Ok(()) };
        let size = lut.size();
        if !(LUT_MIN_SIZE..=LUT_MAX_SIZE).contains(&size)
            || lut.texels.len() != 3 * node_count(size)
        {
            return Err(ModelError("invalid LUT".into()));
        }
        Ok(())
    }
}
impl Validate for LutSlot {
    fn validate(&self) -> Result<(), ModelError> {
        self.check()
    }
}

// ---------------------------------------------------------------------------
// Preset embedding

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum WireOut<'a> {
    None,
    Cube {
        title: &'a str,
        size: usize,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
        encoding: &'static str,
        data: String,
    },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum WireIn {
    None {},
    Cube {
        #[serde(default)]
        title: String,
        size: usize,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
        encoding: String,
        data: String,
    },
}
impl Serialize for LutSlot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::None => WireOut::None.serialize(serializer),
            Self::Cube(lut) => WireOut::Cube {
                title: &lut.title,
                size: lut.size(),
                domain_min: lut.domain_min,
                domain_max: lut.domain_max,
                encoding: EMBED_ENCODING,
                data: lut.to_embedded(),
            }
            .serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for LutSlot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match WireIn::deserialize(deserializer)? {
            WireIn::None {} => Ok(Self::None),
            WireIn::Cube {
                title,
                size,
                domain_min,
                domain_max,
                encoding,
                data,
            } => CubeLut::from_embedded(&title, size, domain_min, domain_max, &encoding, &data)
                .map(|lut| Self::Cube(Arc::new(lut)))
                .map_err(D::Error::custom),
        }
    }
}

const fn corrupt() -> CubeError {
    CubeError::Embedded("LUT data is corrupt")
}
/// Largest compressed payload accepted for `raw_len` raw bytes.
fn compressed_max(raw_len: usize) -> usize {
    raw_len + raw_len / 1000 + 64
}
fn base64_max(raw_len: usize) -> usize {
    4 * compressed_max(raw_len).div_ceil(3)
}
/// Read exactly `raw_len` bytes, producing at most `raw_len + 1` before giving up.
fn read_exact_bounded<R: Read>(reader: R, raw_len: usize) -> Result<Vec<u8>, CubeError> {
    let mut out = Vec::with_capacity(raw_len);
    let limit = raw_len as u64 + 1;
    reader
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => CubeError::Embedded("LUT data is truncated"),
            _ => corrupt(),
        })?;
    match out.len().cmp(&raw_len) {
        std::cmp::Ordering::Less => Err(CubeError::Embedded("LUT data is truncated")),
        std::cmp::Ordering::Greater => Err(CubeError::Embedded("LUT data is larger than declared")),
        std::cmp::Ordering::Equal => Ok(out),
    }
}
fn shuffle(texels: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(texels.len() * 2);
    out.extend(texels.iter().map(|t| t.to_le_bytes()[0]));
    out.extend(texels.iter().map(|t| t.to_le_bytes()[1]));
    out
}
fn unshuffle(bytes: &[u8]) -> Vec<u16> {
    let (low, high) = bytes.split_at(bytes.len() / 2);
    low.iter()
        .zip(high)
        .map(|(l, h)| u16::from_le_bytes([*l, *h]))
        .collect()
}
impl CubeLut {
    /// The `data` field of the embedded form: shuffled f16 bytes, zlib level 6, base64.
    pub fn to_embedded(&self) -> String {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        // Writing to a Vec cannot fail.
        encoder
            .write_all(&shuffle(&self.texels))
            .expect("in-memory zlib write");
        base64::encode(&encoder.finish().expect("in-memory zlib finish"))
    }
    /// Decode an embedded LUT. Header fields are validated before `data` is touched;
    /// every allocation is bounded by `size` whatever the compression ratio.
    pub fn from_embedded(
        title: &str,
        size: usize,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
        encoding: &str,
        data: &str,
    ) -> Result<Self, CubeError> {
        if !(LUT_MIN_SIZE..=LUT_MAX_SIZE).contains(&size) {
            return Err(CubeError::Size(size));
        }
        if !domain_is_valid(domain_min, domain_max) {
            return Err(CubeError::Domain { line: None });
        }
        if encoding != EMBED_ENCODING {
            return Err(CubeError::Embedded("unsupported LUT encoding"));
        }
        let raw_len = node_count(size) * 6;
        if data.len() > base64_max(raw_len) {
            return Err(CubeError::Embedded("LUT data exceeds the size limit"));
        }
        let compressed =
            base64::decode(data).ok_or(CubeError::Embedded("LUT data is not valid base64"))?;
        let mut input: &[u8] = &compressed;
        let bytes = read_exact_bounded(flate2::bufread::ZlibDecoder::new(&mut input), raw_len)?;
        if !input.is_empty() {
            return Err(CubeError::Embedded("LUT data has trailing bytes"));
        }
        Self::new(size, title, domain_min, domain_max, unshuffle(&bytes))
    }
}

/// Strict RFC 4648 base64: standard alphabet, mandatory padding, no whitespace,
/// canonical trailing bits.
mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(super) fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
            let n = b(0) << 16 | b(1) << 8 | b(2);
            for k in 0..4 {
                if k <= chunk.len() {
                    out.push(char::from(ALPHABET[(n >> (18 - 6 * k) & 63) as usize]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
    fn value(c: u8) -> Option<u32> {
        ALPHABET.iter().position(|a| *a == c).map(|v| v as u32)
    }
    pub(super) fn decode(text: &str) -> Option<Vec<u8>> {
        let bytes = text.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let groups = bytes.len() / 4;
        let mut out = Vec::with_capacity(groups * 3);
        for (index, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let pad = if index + 1 == groups {
                chunk.iter().rev().take_while(|c| **c == b'=').count()
            } else {
                0
            };
            if pad > 2 {
                return None;
            }
            let mut n = 0_u32;
            for c in &chunk[..4 - pad] {
                n = n << 6 | value(*c)?;
            }
            n <<= 6 * pad as u32;
            // Unused low bits must be zero, or the encoding is not canonical.
            if n & ((1 << (8 * pad)) - 1) != 0 {
                return None;
            }
            out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
        }
        Some(out)
    }
}

#[cfg(test)]
#[path = "../tests/unit/lut_tests.rs"]
mod tests;
