//! Strict structural decoder for the measured FText histories used by overlays.
//!
//! It preserves wire identifiers and argument trees; it makes no localization
//! or gameplay-meaning inference.
use core::fmt::{self, Write};
use thiserror::Error;
use vrf_bitio::{BitError, BitReader};

const MAX_STRING_BYTES: u64 = 64 * 1024;
const MAX_DEPTH: u16 = 16;
const MAX_NODES: u16 = 256;
const MAX_FORMAT_ARGUMENTS: i32 = 128;

/// A complete measured FText history tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FTextTree {
    /// History 4 (`AsNumber`): a number the game formats when it displays the
    /// text. `BombGameState_C.OverrideMatchTimerText` sends it whenever the
    /// match timer is overridden; a double source (argument type 3) is the
    /// only one observed, and the only one read.
    AsNumber {
        flags: u32,
        /// The source double's bits: exact and comparable (`f64` is not
        /// `Eq`). Always finite -- a non-finite value is refused while
        /// decoding, because JSON has no spelling for it.
        source_bits: u64,
        /// `FNumberFormattingOptions`, when the text carries its own.
        format: Option<FTextNumberFormat>,
        /// The target culture's name; empty for the current culture.
        culture: String,
    },
    /// History 11 string-table form.
    StringTable {
        flags: u32,
        table: FTextName,
        key: String,
    },
    /// History 3 format form. Arguments retain their wire order and duplicates.
    Format {
        flags: u32,
        source: Box<FTextTree>,
        arguments: Vec<FTextArgument>,
    },
    /// The observed empty form: history 255 with zero flags and a zero i32
    /// after the history byte. That i32 is most likely Unreal's
    /// `bHasCultureInvariantString` archive bool rather than a length; a 1
    /// would be followed by a string, and is refused as `InvalidEmptyForm`.
    Empty { flags: u32 },
}
/// `FNumberFormattingOptions` in wire order: two archive bools (whole u32s,
/// 0 or 1), the rounding mode as a signed byte, then four i32 digit limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FTextNumberFormat {
    pub always_sign: bool,
    pub use_grouping: bool,
    pub rounding_mode: i8,
    pub minimum_integral_digits: i32,
    pub maximum_integral_digits: i32,
    pub minimum_fractional_digits: i32,
    pub maximum_fractional_digits: i32,
}
/// Inline FName from a string-table history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FTextName {
    pub name: String,
    pub number: i32,
}
/// One ordered format argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FTextArgument {
    pub name: String,
    pub tag: u8,
    pub value: FTextArgumentValue,
}
/// Explicitly measured argument forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FTextArgumentValue {
    U64Bits(u64),
    Text(Box<FTextTree>),
}
/// Full-tree FText decoding failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FTextTreeError {
    #[error(transparent)]
    BitIo(#[from] BitError),
    #[error("unsupported FText history discriminator {discriminator}")]
    UnsupportedHistory { discriminator: u8 },
    #[error("unsupported FText name form")]
    UnsupportedNameForm,
    #[error("FText FName suffix must be nonnegative, got {number}")]
    NegativeNameSuffix { number: i32 },
    #[error("FText format argument count must be in 0..={max}, got {actual}")]
    InvalidArgumentCount { max: i32, actual: i32 },
    #[error("unsupported FText format argument tag {tag}")]
    UnsupportedArgumentTag { tag: u8 },
    #[error("empty FText history must have zero flags and zero length")]
    InvalidEmptyForm,
    #[error("FText string exceeds {max_bytes} byte cap: {bytes} bytes")]
    StringTooLong { bytes: u64, max_bytes: u64 },
    #[error("FText string is missing its NUL terminator")]
    MissingStringTerminator,
    #[error("FText nesting exceeds depth limit {limit}")]
    DepthLimit { limit: u16 },
    #[error("FText node count exceeds limit {limit}")]
    NodeLimit { limit: u16 },
    #[error("FText payload has {remaining} unconsumed bits")]
    TrailingBits { remaining: u64 },
    /// An archive bool is a whole u32; anything but 0 or 1 is not one.
    #[error("FText archive bool must be 0 or 1, got {value}")]
    InvalidBool { value: u32 },
    /// A history-4 source double that is NaN or infinite has no JSON spelling.
    #[error("FText number is not finite")]
    NonFiniteNumber,
}

/// Decode an exact bit window containing only measured FText histories 11, 3,
/// 4 and the observed 255 empty form. Raw u64 argument bits remain unsigned.
pub fn decode_ftext_tree(data: &[u8], bit_count: u32) -> Result<FTextTree, FTextTreeError> {
    let mut r = BitReader::with_bit_len(data, u64::from(bit_count))?;
    let value = decode_ftext_tree_from(&mut r)?;
    if r.bits_remaining() != 0 {
        return Err(FTextTreeError::TrailingBits {
            remaining: r.bits_remaining(),
        });
    }
    Ok(value)
}
/// [`decode_ftext_tree`] on a reader already at the payload, for
/// `FieldType::FTextTree`. The trailing-bit check is the caller's:
/// `decode_field` refuses leftover bits for every type alike.
pub(crate) fn decode_ftext_tree_from(r: &mut BitReader<'_>) -> Result<FTextTree, FTextTreeError> {
    let mut nodes = 0;
    decode_tree(r, 0, &mut nodes)
}
impl FTextTree {
    /// The wire history byte.
    pub(crate) fn history(&self) -> u8 {
        match self {
            Self::AsNumber { .. } => 4,
            Self::StringTable { .. } => 11,
            Self::Format { .. } => 3,
            Self::Empty { .. } => 255,
        }
    }

    /// JSON for additive `value_str`; it retains flags, history and wire values.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write_json(&mut s).expect("String write");
        s
    }
    fn write_json(&self, s: &mut String) -> fmt::Result {
        match self {
            Self::StringTable { flags, table, key } => {
                write!(
                    s,
                    r#"{{"flags":{flags},"history":11,"kind":"string_table","table":{{"name":"#
                )?;
                json_string(s, &table.name)?;
                write!(s, r#","number":{}}},"key":"#, table.number)?;
                json_string(s, key)?;
                s.push('}');
                Ok(())
            }
            Self::Format {
                flags,
                source,
                arguments,
            } => {
                write!(
                    s,
                    r#"{{"flags":{flags},"history":3,"kind":"format","source":"#
                )?;
                source.write_json(s)?;
                s.push_str(r#","arguments":["#);
                for (i, a) in arguments.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(r#"{"name":"#);
                    json_string(s, &a.name)?;
                    write!(s, r#","tag":{},"value":"#, a.tag)?;
                    match &a.value {
                        FTextArgumentValue::U64Bits(v) => write!(s, r#"{{"bits_u64":"{v}"}}"#)?,
                        FTextArgumentValue::Text(v) => v.write_json(s)?,
                    };
                    s.push('}');
                }
                s.push_str("]}");
                Ok(())
            }
            Self::Empty { flags } => {
                write!(s, r#"{{"flags":{flags},"history":255,"kind":"empty"}}"#)
            }
            Self::AsNumber {
                flags,
                source_bits,
                format,
                culture,
            } => {
                // `{}`: shortest round-trip spelling, never exponent form.
                write!(
                    s,
                    r#"{{"flags":{flags},"history":4,"kind":"as_number","source":{{"tag":3,"double":{}}},"format":"#,
                    f64::from_bits(*source_bits)
                )?;
                match format {
                    Some(f) => write!(
                        s,
                        r#"{{"always_sign":{},"use_grouping":{},"rounding_mode":{},"minimum_integral_digits":{},"maximum_integral_digits":{},"minimum_fractional_digits":{},"maximum_fractional_digits":{}}}"#,
                        f.always_sign,
                        f.use_grouping,
                        f.rounding_mode,
                        f.minimum_integral_digits,
                        f.maximum_integral_digits,
                        f.minimum_fractional_digits,
                        f.maximum_fractional_digits
                    )?,
                    None => s.push_str("null"),
                }
                s.push_str(r#","culture":"#);
                json_string(s, culture)?;
                s.push('}');
                Ok(())
            }
        }
    }
}
/// Charge one node against the total-node budget: each tree (each
/// [`decode_tree`] call) and each format argument. `checked_add` keeps the
/// check sound on its own, not only while `MAX_NODES` stays below `u16::MAX`.
fn charge_node_budget(nodes: &mut u16) -> Result<(), FTextTreeError> {
    *nodes = nodes
        .checked_add(1)
        .ok_or(FTextTreeError::NodeLimit { limit: MAX_NODES })?;
    if *nodes > MAX_NODES {
        return Err(FTextTreeError::NodeLimit { limit: MAX_NODES });
    }
    Ok(())
}

fn decode_tree(
    r: &mut BitReader<'_>,
    depth: u16,
    nodes: &mut u16,
) -> Result<FTextTree, FTextTreeError> {
    if depth >= MAX_DEPTH {
        return Err(FTextTreeError::DepthLimit { limit: MAX_DEPTH });
    }
    charge_node_budget(nodes)?;
    let flags = r.read_bits(32)? as u32;
    let history = r.read_bits(8)? as u8;
    match history {
        11 => {
            if r.read_bit()? {
                return Err(FTextTreeError::UnsupportedNameForm);
            };
            let name = read_string(r)?;
            let number = r.read_i32()?;
            if number < 0 {
                return Err(FTextTreeError::NegativeNameSuffix { number });
            };
            let key = read_string(r)?;
            Ok(FTextTree::StringTable {
                flags,
                table: FTextName { name, number },
                key,
            })
        }
        3 => {
            let source = Box::new(decode_tree(r, depth + 1, nodes)?);
            let actual = r.read_i32()?;
            if !(0..=MAX_FORMAT_ARGUMENTS).contains(&actual) {
                return Err(FTextTreeError::InvalidArgumentCount {
                    max: MAX_FORMAT_ARGUMENTS,
                    actual,
                });
            };
            let mut arguments = Vec::with_capacity(actual as usize);
            for _ in 0..actual {
                charge_node_budget(nodes)?;
                let name = read_string(r)?;
                let tag = r.read_bits(8)? as u8;
                let value = match tag {
                    0 => FTextArgumentValue::U64Bits(r.read_u64()?),
                    4 => FTextArgumentValue::Text(Box::new(decode_tree(r, depth + 1, nodes)?)),
                    _ => return Err(FTextTreeError::UnsupportedArgumentTag { tag }),
                };
                arguments.push(FTextArgument { name, tag, value });
            }
            Ok(FTextTree::Format {
                flags,
                source,
                arguments,
            })
        }
        4 => {
            // `FFormatArgumentValue`: a type byte, then the value. Only a
            // double (3) has been observed; any other type is refused rather
            // than read with a guessed width.
            let tag = r.read_bits(8)? as u8;
            if tag != 3 {
                return Err(FTextTreeError::UnsupportedArgumentTag { tag });
            }
            let source = r.read_f64()?;
            if !source.is_finite() {
                return Err(FTextTreeError::NonFiniteNumber);
            }
            let format = if read_archive_bool(r)? {
                Some(FTextNumberFormat {
                    always_sign: read_archive_bool(r)?,
                    use_grouping: read_archive_bool(r)?,
                    rounding_mode: r.read_bits(8)? as u8 as i8,
                    minimum_integral_digits: r.read_i32()?,
                    maximum_integral_digits: r.read_i32()?,
                    minimum_fractional_digits: r.read_i32()?,
                    maximum_fractional_digits: r.read_i32()?,
                })
            } else {
                None
            };
            let culture = read_string(r)?;
            Ok(FTextTree::AsNumber {
                flags,
                source_bits: source.to_bits(),
                format,
                culture,
            })
        }
        255 => {
            if flags != 0 || r.read_i32()? != 0 {
                Err(FTextTreeError::InvalidEmptyForm)
            } else {
                Ok(FTextTree::Empty { flags })
            }
        }
        _ => Err(FTextTreeError::UnsupportedHistory {
            discriminator: history,
        }),
    }
}
/// An archive `bool`: Unreal serializes it as a whole u32.
fn read_archive_bool(r: &mut BitReader<'_>) -> Result<bool, FTextTreeError> {
    match r.read_u32()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(FTextTreeError::InvalidBool { value }),
    }
}
/// A length past the payload is `InvalidLength`, as `read_fstring` reports it.
fn read_string(r: &mut BitReader<'_>) -> Result<String, FTextTreeError> {
    let start = r.position();
    let length = r.read_i32()?;
    if length == 0 {
        return Ok(String::new());
    };
    // A positive length counts UTF-8 bytes, a negative one UTF-16 units.
    let wide = length < 0;
    let units = u64::from(length.unsigned_abs());
    let bytes = units.saturating_mul(if wide { 2 } else { 1 });
    if bytes > MAX_STRING_BYTES {
        return Err(FTextTreeError::StringTooLong {
            bytes,
            max_bytes: MAX_STRING_BYTES,
        });
    };
    if bytes * 8 > r.bits_remaining() {
        let length = i64::from(length);
        return Err(BitError::InvalidLength {
            position: start,
            length,
        }
        .into());
    }
    let width = if wide { 16 } else { 8 };
    let mut v = Vec::with_capacity(units as usize);
    for _ in 0..units {
        v.push(r.read_bits(width)? as u16);
    }
    if v.pop() != Some(0) {
        return Err(FTextTreeError::MissingStringTerminator);
    };
    let text = if wide {
        String::from_utf16(&v).ok()
    } else {
        String::from_utf8(v.into_iter().map(|unit| unit as u8).collect()).ok()
    };
    text.ok_or_else(|| BitError::InvalidString { position: start }.into())
}
fn json_string(s: &mut String, value: &str) -> fmt::Result {
    s.push('"');
    for c in value.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if c <= '\u{1f}' => write!(s, "\\u{:04x}", c as u32)?,
            c => s.push(c),
        }
    }
    s.push('"');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vrf_testkit::{BitWrite, BitWriter};

    /// The FText pieces these tests assemble, on the shared writer.
    trait FTextBits {
        fn string(&mut self, value: &str) -> &mut Self;
        fn table(&mut self, flags: u32, name: &str, number: i32, key: &str) -> &mut Self;
        fn empty(&mut self) -> &mut Self;
    }

    impl FTextBits for BitWriter {
        fn string(&mut self, value: &str) -> &mut Self {
            self.i32((value.len() + 1) as i32);
            for byte in value.bytes() {
                self.bits(u64::from(byte), 8);
            }
            self.bits(0, 8)
        }
        fn table(&mut self, flags: u32, name: &str, number: i32, key: &str) -> &mut Self {
            self.bits(u64::from(flags), 32).bits(11, 8).bits(0, 1);
            self.string(name).i32(number).string(key)
        }
        fn empty(&mut self) -> &mut Self {
            self.bits(0, 32).bits(255, 8).i32(0)
        }
    }

    #[test]
    fn string_table_preserves_suffix_and_escapes_json() {
        let mut bits = BitWriter::new();
        bits.table(7, "Table\\Name", 2, "line\n\"key");
        let (raw, count) = bits.finish();
        let tree = decode_ftext_tree(&raw, count).unwrap();
        assert_eq!(
            tree.to_json(),
            r#"{"flags":7,"history":11,"kind":"string_table","table":{"name":"Table\\Name","number":2},"key":"line\n\"key"}"#
        );
    }

    #[test]
    fn format_keeps_duplicate_names_and_unsigned_bits() {
        let mut bits = BitWriter::new();
        bits.bits(9, 32);
        bits.bits(3, 8);
        bits.table(0, "T", 0, "Source");
        bits.i32(2);
        bits.string("same");
        bits.bits(0, 8);
        bits.bits(u64::MAX, 64);
        bits.string("same");
        bits.bits(4, 8);
        bits.empty();
        let (raw, count) = bits.finish();
        let json = decode_ftext_tree(&raw, count).unwrap().to_json();
        assert!(json.contains(r#""bits_u64":"18446744073709551615""#));
        assert_eq!(json.matches(r#""name":"same""#).count(), 2);
    }

    #[test]
    fn format_accepts_zero_one_and_two_arguments() {
        for count in 0..=2 {
            let mut bits = BitWriter::new();
            bits.bits(0, 32);
            bits.bits(3, 8);
            bits.empty();
            bits.i32(count);
            for index in 0..count {
                bits.string("arg");
                bits.bits(0, 8);
                bits.bits(index as u64, 64);
            }
            let (raw, width) = bits.finish();
            let FTextTree::Format { arguments, .. } = decode_ftext_tree(&raw, width).unwrap()
            else {
                panic!("format history");
            };
            assert_eq!(arguments.len(), count as usize);
        }
    }

    #[test]
    fn rejects_bad_terminator_count_tag_flags_and_residual() {
        let mut terminator = BitWriter::new();
        terminator.bits(0, 32);
        terminator.bits(11, 8);
        terminator.bits(0, 1);
        terminator.i32(2);
        terminator.bits(u64::from(b'A'), 8);
        terminator.bits(u64::from(b'X'), 8);
        let (raw, count) = terminator.finish();
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::MissingStringTerminator)
        ));
        let mut count_bits = BitWriter::new();
        count_bits.bits(0, 32);
        count_bits.bits(3, 8);
        count_bits.empty();
        count_bits.i32(-1);
        let (raw, count) = count_bits.finish();
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::InvalidArgumentCount { .. })
        ));
        let mut too_wide = BitWriter::new();
        too_wide.bits(0, 32);
        too_wide.bits(3, 8);
        too_wide.empty();
        too_wide.i32(129);
        let (raw, count) = too_wide.finish();
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::InvalidArgumentCount { .. })
        ));
        let mut tag_bits = BitWriter::new();
        tag_bits.bits(0, 32);
        tag_bits.bits(3, 8);
        tag_bits.empty();
        tag_bits.i32(2);
        tag_bits.string("first");
        tag_bits.bits(0, 8);
        tag_bits.bits(1, 64);
        tag_bits.string("second");
        tag_bits.bits(99, 8);
        let (raw, count) = tag_bits.finish();
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::UnsupportedArgumentTag { tag: 99 })
        ));
        let mut empty = BitWriter::new();
        empty.bits(1, 32);
        empty.bits(255, 8);
        empty.i32(0);
        let (raw, count) = empty.finish();
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::InvalidEmptyForm)
        ));
        let mut residual = BitWriter::new();
        residual.empty();
        residual.bits(1, 1);
        let (raw, count) = residual.finish();
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::TrailingBits { .. })
        ));
    }

    /// A real `OverrideMatchTimerText` payload (13.06, 376 bits): flags 1,
    /// history 4, a double source, format options present, empty culture.
    const TIMER_TEXT: [u8; 47] = [
        0x01, 0x00, 0x00, 0x00, 0x04, 0x03, 0x00, 0x00, 0x00, 0x80, 0x5f, 0x3a, 0x2f, 0x40, 0x01,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
        0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00,
    ];

    #[test]
    fn as_number_keeps_the_measured_timer_text_exact() {
        let tree = decode_ftext_tree(&TIMER_TEXT, 376).unwrap();
        let FTextTree::AsNumber {
            flags,
            source_bits,
            format,
            ref culture,
        } = tree
        else {
            panic!("history 4: {tree:?}");
        };
        assert_eq!(
            (flags, f64::from_bits(source_bits)),
            (1, 15.614009857177734)
        );
        assert_eq!(
            format,
            Some(FTextNumberFormat {
                always_sign: false,
                use_grouping: true,
                rounding_mode: 0,
                minimum_integral_digits: 2,
                maximum_integral_digits: 2,
                minimum_fractional_digits: 2,
                maximum_fractional_digits: 2,
            })
        );
        assert_eq!(culture, "");
        assert_eq!(
            tree.to_json(),
            r#"{"flags":1,"history":4,"kind":"as_number","source":{"tag":3,"double":15.614009857177734},"format":{"always_sign":false,"use_grouping":true,"rounding_mode":0,"minimum_integral_digits":2,"maximum_integral_digits":2,"minimum_fractional_digits":2,"maximum_fractional_digits":2},"culture":""}"#
        );
        // One byte short is a truncation, one byte over is residue.
        assert!(matches!(
            decode_ftext_tree(&TIMER_TEXT[..46], 368),
            Err(FTextTreeError::BitIo(_))
        ));
        let mut long = TIMER_TEXT.to_vec();
        long.push(0);
        assert!(matches!(
            decode_ftext_tree(&long, 384),
            Err(FTextTreeError::TrailingBits { remaining: 8 })
        ));
    }

    #[test]
    fn as_number_reads_the_no_options_branch_and_a_culture() {
        let mut bits = BitWriter::new();
        bits.bits(0, 32);
        bits.bits(4, 8);
        bits.bits(3, 8);
        bits.bits((-2.5f64).to_bits(), 64);
        bits.bits(0, 32);
        bits.string("ko-KR");
        let (raw, count) = bits.finish();
        assert_eq!(
            decode_ftext_tree(&raw, count).unwrap().to_json(),
            r#"{"flags":0,"history":4,"kind":"as_number","source":{"tag":3,"double":-2.5},"format":null,"culture":"ko-KR"}"#
        );
    }

    /// The option members in Unreal's `FNumberFormattingOptions` order. The
    /// corpus cannot pin it -- every observed digit limit is 2 -- so distinct
    /// values do, and a negative rounding byte stays signed.
    #[test]
    fn as_number_format_options_keep_their_wire_order() {
        let mut bits = BitWriter::new();
        bits.bits(0, 32);
        bits.bits(4, 8);
        bits.bits(3, 8);
        bits.bits(1.0f64.to_bits(), 64);
        bits.bits(1, 32);
        bits.bits(1, 32);
        bits.bits(0, 32);
        bits.bits(0xff, 8);
        for limit in [1, 2, 3, 4] {
            bits.i32(limit);
        }
        bits.i32(0);
        let (raw, count) = bits.finish();
        let FTextTree::AsNumber { format, .. } = decode_ftext_tree(&raw, count).unwrap() else {
            panic!("history 4");
        };
        assert_eq!(
            format,
            Some(FTextNumberFormat {
                always_sign: true,
                use_grouping: false,
                rounding_mode: -1,
                minimum_integral_digits: 1,
                maximum_integral_digits: 2,
                minimum_fractional_digits: 3,
                maximum_fractional_digits: 4,
            })
        );
    }

    #[test]
    fn as_number_refuses_other_sources_bad_bools_and_non_finite_values() {
        let number = |tag: u64, value: f64, has_format: u64, always_sign: u64| {
            let mut bits = BitWriter::new();
            bits.bits(1, 32);
            bits.bits(4, 8);
            bits.bits(tag, 8);
            bits.bits(value.to_bits(), 64);
            bits.bits(has_format, 32);
            if has_format == 1 {
                bits.bits(always_sign, 32);
                bits.bits(1, 32);
                bits.bits(0, 8);
                for _ in 0..4 {
                    bits.i32(2);
                }
            }
            bits.i32(0);
            bits.finish()
        };
        let (raw, count) = number(3, 1.5, 1, 0);
        assert!(decode_ftext_tree(&raw, count).is_ok());
        // A float source (2) is four bytes, not eight: refused, not misread.
        let (raw, count) = number(2, 1.5, 1, 0);
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::UnsupportedArgumentTag { tag: 2 })
        ));
        let (raw, count) = number(3, f64::NAN, 1, 0);
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::NonFiniteNumber)
        ));
        let (raw, count) = number(3, 1.5, 2, 0);
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::InvalidBool { value: 2 })
        ));
        let (raw, count) = number(3, 1.5, 1, 7);
        assert!(matches!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::InvalidBool { value: 7 })
        ));
    }

    #[test]
    fn unicode_strings_are_strict_and_byte_bounded() {
        let mut valid = BitWriter::new();
        valid.bits(0, 32);
        valid.bits(11, 8);
        valid.bits(0, 1);
        valid.i32(-3);
        for unit in [0xd83d, 0xde00, 0] {
            valid.bits(unit, 16);
        }
        valid.i32(0);
        valid.string("Key");
        let (raw, count) = valid.finish();
        let FTextTree::StringTable { table, .. } = decode_ftext_tree(&raw, count).unwrap() else {
            panic!("string table");
        };
        assert_eq!(table.name, "\u{1f600}");

        for (length, units, width) in [(2, vec![0xff, 0], 8), (-2, vec![0xd800, 0], 16)] {
            let mut invalid = BitWriter::new();
            invalid.bits(0, 32);
            invalid.bits(11, 8);
            invalid.bits(0, 1);
            invalid.i32(length);
            for unit in units {
                invalid.bits(unit, width);
            }
            let (raw, count) = invalid.finish();
            assert!(matches!(
                decode_ftext_tree(&raw, count),
                Err(FTextTreeError::BitIo(BitError::InvalidString { .. }))
            ));
        }
        for length in [65_537, -32_769, i32::MIN] {
            let mut invalid = BitWriter::new();
            invalid.bits(0, 32);
            invalid.bits(11, 8);
            invalid.bits(0, 1);
            invalid.i32(length);
            let (raw, count) = invalid.finish();
            assert!(matches!(
                decode_ftext_tree(&raw, count),
                Err(FTextTreeError::StringTooLong { .. })
            ));
        }
        // Under the cap but past the window: the prefix is at fault, not an EOF.
        let mut past = BitWriter::new();
        past.bits(0, 32)
            .bits(11, 8)
            .bits(0, 1)
            .i32(1000)
            .repeat(false, 128);
        let (raw, count) = past.finish();
        assert_eq!(
            decode_ftext_tree(&raw, count),
            Err(FTextTreeError::BitIo(BitError::InvalidLength {
                position: 41,
                length: 1000
            }))
        );
    }

    #[test]
    fn depth_and_total_node_budgets_reject_otherwise_complete_trees() {
        for levels in [MAX_DEPTH - 1, MAX_DEPTH] {
            let mut nested = BitWriter::new();
            for _ in 0..levels {
                nested.bits(0, 32);
                nested.bits(3, 8);
            }
            nested.empty();
            for _ in 0..levels {
                nested.i32(0);
            }
            let (raw, count) = nested.finish();
            if levels < MAX_DEPTH {
                assert!(decode_ftext_tree(&raw, count).is_ok());
            } else {
                assert!(matches!(
                    decode_ftext_tree(&raw, count),
                    Err(FTextTreeError::DepthLimit { .. })
                ));
            }
        }
        // Root + source + (one argument and one text per entry): the final
        // argument crosses the global budget while its local count is valid.
        for arguments in [127, 128] {
            let mut wide = BitWriter::new();
            wide.bits(0, 32);
            wide.bits(3, 8);
            wide.empty();
            wide.i32(arguments);
            for _ in 0..arguments {
                wide.string("arg");
                wide.bits(4, 8);
                wide.empty();
            }
            let (raw, count) = wide.finish();
            if arguments == 127 {
                assert!(decode_ftext_tree(&raw, count).is_ok());
            } else {
                assert!(matches!(
                    decode_ftext_tree(&raw, count),
                    Err(FTextTreeError::NodeLimit { .. })
                ));
            }
        }
    }
}
