/// PDF content streams.
use std::fmt::{self, Display};
use std::cmp::Ordering;
use std::ops::Mul;
use itertools::Itertools;

use crate::error::*;
use crate::object::*;
use crate::parser::{Lexer, parse_with_lexer};
use crate::primitive::*;
use crate::enc::StreamFilter;

/// Represents a PDF content stream - a `Vec` of `Operator`s
#[derive(Debug, Clone)]
pub struct Content {
    /// The raw content stream parts. usually one, but could be any number.
    pub parts: Vec<Stream<()>>,

    /// The parsed operations. You probably want to use these.
    pub operations: Vec<Op>,
}

macro_rules! names {
    ($args:ident, $($x:ident),*) => (
        $(
            let $x = name(&mut $args)?;
        )*
    )
}
macro_rules! numbers {
    ($args:ident, $($x:ident),*) => (
        $(
            let $x = number(&mut $args)?;
        )*
    )
}
macro_rules! points {
    ($args:ident, $($point:ident),*) => (
        $(
            let $point = point(&mut $args)?;
        )*
    )
}
fn name(args: &mut impl Iterator<Item=Primitive>) -> Result<String> {
    args.next().ok_or(PdfError::NoOpArg)?.into_name()
}
fn number(args: &mut impl Iterator<Item=Primitive>) -> Result<f32> {
    args.next().ok_or(PdfError::NoOpArg)?.as_number()
}
fn string(args: &mut impl Iterator<Item=Primitive>) -> Result<PdfString> {
    args.next().ok_or(PdfError::NoOpArg)?.into_string()
}
fn point(args: &mut impl Iterator<Item=Primitive>) -> Result<Point> {
    let x = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let y = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    Ok(Point { x, y })
}
fn rect(args: &mut impl Iterator<Item=Primitive>) -> Result<Rect> {
    let x = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let y = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let width = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let height = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    Ok(Rect { x, y, width, height })
}
fn rgb(args: &mut impl Iterator<Item=Primitive>) -> Result<Rgb> {
    let red = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let green = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let blue = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    Ok(Rgb { red, green, blue })
}
fn cmyk(args: &mut impl Iterator<Item=Primitive>) -> Result<Cmyk> {
    let cyan = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let magenta = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let yellow = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    let key = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
    Ok(Cmyk { cyan, magenta, yellow, key })
}
fn matrix(args: &mut impl Iterator<Item=Primitive>) -> Result<Matrix> {
    Ok(Matrix {
        a: number(args)?,
        b: number(args)?,
        c: number(args)?,
        d: number(args)?,
        e: number(args)?,
        f: number(args)?,
    })
}
fn array(args: &mut impl Iterator<Item=Primitive>) -> Result<Vec<Primitive>> {
    match args.next() {
        Some(Primitive::Array(arr)) => Ok(arr),
        _ => Err(PdfError::NoOpArg)
    }
}

fn expand_abbr_name(name: String, alt: &[(&str, &str)]) -> String {
    for &(p, r) in alt {
        if name == p {
            return r.into();
        }
    }
    name
}
fn expand_abbr(p: Primitive, alt: &[(&str, &str)]) -> Primitive {
    match p {
        Primitive::Name(name) => Primitive::Name(expand_abbr_name(name, alt)),
        Primitive::Array(items) => Primitive::Array(items.into_iter().map(|p| expand_abbr(p, alt)).collect()),
        p => p
    }
}

fn inline_image(lexer: &mut Lexer, resolve: &impl Resolve) -> Result<Stream<ImageDict>> {
    let mut dict = Dictionary::new();
    loop {
        let backup_pos = lexer.get_pos();
        let obj = parse_with_lexer(lexer, &NoResolve);
        let key = match obj {
            Ok(Primitive::Name(key)) => key,
            Err(e) if e.is_eof() => return Err(e),
            Err(_) => {
                lexer.set_pos(backup_pos);
                break;
            }
            Ok(_) => bail!("invalid key type")
        };
        let key = expand_abbr_name(key, &[
            ("BPC", "BitsPerComponent"),
            ("CS", "ColorSpace"),
            ("D", "Decode"),
            ("DP", "DecodeParms"),
            ("F", "Filter"),
            ("H", "Height"),
            ("IM", "ImageMask"),
            ("I", "Interpolate"),
            ("W", "Width"),
        ]);
        let val = parse_with_lexer(lexer, &NoResolve)?;
        dict.insert(key, val);
    }
    lexer.next_expect("ID")?;
    let data_start = lexer.get_pos() + 1;

    // ugh
    let bits_per_component = dict.require("InlineImage", "BitsPerComponent")?.as_integer()?;
    let color_space = expand_abbr(
        dict.require("InlineImage", "ColorSpace")?,
        &[
            ("G", "DeviceGray"),
            ("RGB", "DeviceRGB"),
            ("CMYK", "DeviceCMYK"),
            ("I", "Indexed")
        ]
    );
    let decode = Object::from_primitive(dict.require("InlineImage", "Decode")?, resolve)?;
    let decode_parms = dict.require("InlineImage", "DecodeParms")?.into_dictionary(resolve)?;
    let filter = expand_abbr(
        dict.require("InlineImage", "Filter")?,
        &[
            ("AHx", "ASCIIHexDecode"),
            ("A85", "ASCII85Decode"),
            ("LZW", "LZWDecode"),
            ("Fl", "FlateDecode"),
            ("RL", "RunLengthDecode"),
            ("CCF", "CCITTFaxDecode"),
            ("DCT", "DCTDecode"),
        ]
    );
    let filters = match filter {
        Primitive::Array(parts) => parts.into_iter()
            .map(|p| p.as_name().and_then(|kind| StreamFilter::from_kind_and_params(kind, decode_parms.clone(), resolve)))
            .collect::<Result<_>>()?,
        Primitive::Name(kind) => vec![StreamFilter::from_kind_and_params(&kind, decode_parms, resolve)?],
        _ => bail!("invalid filter")
    };
    
    let height = dict.require("InlineImage", "Height")?.as_integer()?;
    let image_mask = dict.get("ImageMask").map(|p| p.as_bool()).transpose()?.unwrap_or(false);
    let intent = dict.remove("Intent").map(|p| RenderingIntent::from_primitive(p, &NoResolve)).transpose()?;
    let interpolate = dict.get("Interpolate").map(|p| p.as_bool()).transpose()?.unwrap_or(false);
    let width = dict.require("InlineImage", "Width")?.as_integer()?;

    let image_dict = ImageDict {
        width,
        height,
        color_space: Some(color_space),
        bits_per_component,
        intent,
        image_mask,
        mask: None,
        decode,
        interpolate,
        struct_parent: None,
        id: None,
        smask: None,
        other: dict,
    };

    lexer.seek_substr("\nEI").expect("BUGZ");
    let data_end = lexer.get_pos() - 3;

    let data = lexer.new_substr(data_start .. data_end).to_vec();

    Ok(Stream::new_with_filters(image_dict, data, filters))
}
struct OpBuilder {
    last: Point,
    compability_section: bool,
    ops: Vec<Op>
}
impl OpBuilder {
    fn new() -> Self {
        OpBuilder {
            last: Point { x: 0., y: 0. },
            compability_section: false,
            ops: Vec::new()
        }
    }
    fn parse(&mut self, data: &[u8], resolve: &impl Resolve) -> Result<()> {
        let mut lexer = Lexer::new(data);
        let mut buffer = Vec::with_capacity(5);

        loop {
            let backup_pos = lexer.get_pos();
            let obj = parse_with_lexer(&mut lexer, resolve);
            match obj {
                Ok(obj) => {
                    // Operand
                    buffer.push(obj)
                }
                Err(e) => {
                    if e.is_eof() {
                        break;
                    }
                    // It's not an object/operand - treat it as an operator.
                    lexer.set_pos(backup_pos);
                    let op = t!(lexer.next());
                    let operator = t!(op.as_str());
                    t!(self.add(operator, buffer.drain(..), &mut lexer, resolve));
                }
            }
            match lexer.get_pos().cmp(&data.len()) {
                Ordering::Greater => err!(PdfError::ContentReadPastBoundary),
                Ordering::Less => (),
                Ordering::Equal => break
            }
        }
        Ok(())
    }
    fn add(&mut self, op: &str, mut args: impl Iterator<Item=Primitive>, lexer: &mut Lexer, resolve: &impl Resolve) -> Result<()> {
        use Winding::*;

        let ops = &mut self.ops;
        let mut push = move |op| ops.push(op);

        match op {
            "b"   => {
                push(Op::Close);
                push(Op::FillAndStroke { winding: NonZero });
            },
            "B"   => push(Op::FillAndStroke { winding: NonZero }),
            "b*"  => {
                push(Op::Close);
                push(Op::FillAndStroke { winding: EvenOdd });
            }
            "B*"  => push(Op::FillAndStroke { winding: EvenOdd }),
            "BDC" => push(Op::BeginMarkedContent {
                tag: name(&mut args)?,
                properties: Some(args.next().ok_or(PdfError::NoOpArg)?)
            }),
            "BI"  => push(Op::InlineImage { image: inline_image(lexer, resolve)? }),
            "BMC" => push(Op::BeginMarkedContent {
                tag: name(&mut args)?,
                properties: None
            }),
            "BT"  => push(Op::BeginText),
            "BX"  => self.compability_section = true,
            "c"   => {
                points!(args, c1, c2, p);
                push(Op::CurveTo { c1, c2, p });
                self.last = p;
            }
            "cm"  => {
                numbers!(args, a, b, c, d, e, f);
                push(Op::Transform { matrix: Matrix { a, b, c, d, e, f }});
            }
            "CS"  => {
                names!(args, name);
                push(Op::StrokeColorSpace { name });
            }
            "cs"  => {
                names!(args, name);
                push(Op::FillColorSpace { name });
            }
            "d"  => {
                let p = args.next().ok_or(PdfError::NoOpArg)?;
                let pattern = p.as_array()?.iter().map(|p| p.as_number()).collect::<Result<Vec<f32>, PdfError>>()?;
                let phase = args.next().ok_or(PdfError::NoOpArg)?.as_number()?;
                push(Op::Dash { pattern, phase });
            }
            "d0"  => {}
            "d1"  => {}
            "Do"  => {
                names!(args, name);
                push(Op::XObject { name });
            }
            "DP"  => push(Op::MarkedContentPoint {
                tag: name(&mut args)?,
                properties: Some(args.next().ok_or(PdfError::NoOpArg)?)
            }),
            "EI"  => bail!("Parse Error. Unexpected 'EI'"),
            "EMC" => push(Op::EndMarkedContent),
            "ET"  => push(Op::EndText),
            "EX"  => self.compability_section = false,
            "f" |
            "F"   => push(Op::Fill { winding: NonZero }),
            "f*"  => push(Op::Fill { winding: EvenOdd }),
            "G"   => push(Op::StrokeColor { color: Color::Gray(number(&mut args)?) }),
            "g"   => push(Op::FillColor { color: Color::Gray(number(&mut args)?) }),
            "gs"  => push(Op::GraphicsState { name: name(&mut args)? }),
            "h"   => push(Op::Close),
            "i"   => push(Op::Flatness { tolerance: number(&mut args)? }),
            "ID"  => bail!("Parse Error. Unexpected 'ID'"),
            "j"   => {
                let n = args.next().ok_or(PdfError::NoOpArg)?.as_integer()?;
                let join = match n {
                    0 => LineJoin::Miter,
                    1 => LineJoin::Round,
                    2 => LineJoin::Bevel,
                    _ => bail!("invalid line join {}", n)
                };
                push(Op::LineJoin { join });
            }
            "J"   => {
                let n = args.next().ok_or(PdfError::NoOpArg)?.as_integer()?;
                let cap = match n {
                    0 => LineCap::Butt,
                    1 => LineCap::Round,
                    2 => LineCap::Square,
                    _ => bail!("invalid line cap {}", n)
                };
                push(Op::LineCap { cap });
            }
            "K"   => {
                let color = Color::Cmyk(cmyk(&mut args)?);
                push(Op::StrokeColor { color });
            }
            "k"   => {
                let color = Color::Cmyk(cmyk(&mut args)?);
                push(Op::FillColor { color });
            }
            "l"   => {
                let p = point(&mut args)?;
                push(Op::LineTo { p });
                self.last = p;
            }
            "m"   => {
                let p = point(&mut args)?;
                push(Op::MoveTo { p });
                self.last = p;
            }
            "M"   => push(Op::MiterLimit { limit: number(&mut args)? }),
            "MP"  => push(Op::MarkedContentPoint { tag: name(&mut args)?, properties: None }),
            "n"   => push(Op::EndPath),
            "q"   => push(Op::Save),
            "Q"   => push(Op::Restore),
            "re"  => push(Op::Rect { rect: rect(&mut args)? }),
            "RG"  => push(Op::StrokeColor { color: Color::Rgb(rgb(&mut args)?) }),
            "rg"  => push(Op::FillColor { color: Color::Rgb(rgb(&mut args)?) }),
            "ri"  => {
                let s = name(&mut args)?;
                let intent = RenderingIntent::from_str(&s)
                    .ok_or_else(|| PdfError::Other { msg: format!("invalid rendering intent {}", s) })?;
                push(Op::RenderingIntent { intent });
            },
            "s"   => {
                push(Op::Close);
                push(Op::Stroke);
            }
            "S"   => push(Op::Stroke),
            "SC" | "SCN" => {
                push(Op::StrokeColor { color: Color::Other(args.collect()) });
            }
            "sc" | "scn" => {
                push(Op::FillColor { color: Color::Other(args.collect()) });
            }
            "sh"  => {

            }
            "T*"  => push(Op::TextNewline),
            "Tc"  => push(Op::CharSpacing { char_space: number(&mut args)? }),
            "Td"  => push(Op::MoveTextPosition { translation: point(&mut args)? }),
            "TD"  => {
                let translation = point(&mut args)?;
                push(Op::Leading { leading: -translation.y });
                push(Op::MoveTextPosition { translation });
            }
            "Tf"  => push(Op::TextFont { name: name(&mut args)?, size: number(&mut args)? }),
            "Tj"  => push(Op::TextDraw { text: string(&mut args)? }),
            "TJ"  => push(Op::TextDrawAdjusted { array: array(&mut args)? }),
            "TL"  => push(Op::Leading { leading: number(&mut args)? }),
            "Tm"  => push(Op::SetTextMatrix { matrix: matrix(&mut args)? }), 
            "Tr"  => {
                use TextMode::*;

                let n = args.next().ok_or(PdfError::NoOpArg)?.as_integer()?;
                let mode = match n {
                    0 => Fill,
                    1 => Stroke,
                    2 => FillThenStroke,
                    3 => Invisible,
                    4 => FillAndClip,
                    5 => StrokeAndClip,
                    _ => {
                        bail!("Invalid text render mode: {}", n);
                    }
                };
                push(Op::TextRenderMode { mode });
            }
            "Ts"  => push(Op::TextRise { rise: number(&mut args)? }),
            "Tw"  => push(Op::WordSpacing { word_space: number(&mut args)? }),
            "Tz"  => push(Op::TextScaling { horiz_scale: number(&mut args)? }),
            "v"   => {
                points!(args, c2, p);
                push(Op::CurveTo { c1: self.last, c2, p });
                self.last = p;
            }
            "w"   => push(Op::LineWidth { width: number(&mut args)? }),
            "W"   => push(Op::Clip { winding: NonZero }),
            "W*"  => push(Op::Clip { winding: EvenOdd }),
            "y"   => {
                points!(args, c1, p);
                push(Op::CurveTo { c1, c2: p, p });
                self.last = p;
            }
            "'"   => {
                push(Op::TextNewline);
                push(Op::TextDraw { text: string(&mut args)? });
            }
            "\""  => {
                push(Op::WordSpacing { word_space: number(&mut args)? });
                push(Op::CharSpacing { char_space: number(&mut args)? });
                push(Op::TextNewline);
                push(Op::TextDraw { text: string(&mut args)? });
            }
            o if !self.compability_section => {
                bail!("invalid operator {}", o)
            },
            _ => {}
        }
        Ok(())
    }
}

impl Object for Content {
    /// Convert primitive to Self
    fn from_primitive(p: Primitive, resolve: &impl Resolve) -> Result<Self> {
        type ContentStream = Stream<()>;
        let mut ops = OpBuilder::new();
        let mut parts: Vec<ContentStream> = vec![];

        match p {
            Primitive::Array(arr) => {
                for p in arr {
                    let part = t!(ContentStream::from_primitive(p, resolve));
                    let data = t!(part.data());
                    ops.parse(&data, resolve)?;
                    parts.push(part);
                }
            }
            Primitive::Reference(r) => return Self::from_primitive(t!(resolve.resolve(r)), resolve),
            p => {
                let part = t!(ContentStream::from_primitive(p, resolve));
                let data = t!(part.data());
                ops.parse(&data, resolve)?;
                parts.push(part);
            }
        }

        Ok(Content { operations: ops.ops, parts })
    }
}

#[derive(Debug)]
pub struct FormXObject {
    pub operations: Vec<Op>,
    pub stream: Stream<FormDict>,
}
impl FormXObject {
    pub fn dict(&self) -> &FormDict {
        &self.stream.info.info
    }
}
impl Object for FormXObject {
    /// Convert primitive to Self
    fn from_primitive(p: Primitive, resolve: &impl Resolve) -> Result<Self> {
        let stream = t!(Stream::<FormDict>::from_primitive(p, resolve));
        let mut ops = OpBuilder::new();
        ops.parse(stream.data()?, resolve)?;
        Ok(FormXObject {
            stream,
            operations: ops.ops
        })
    }
}


fn serialize_ops(mut ops: &[Op]) -> Result<Vec<u8>> {
    use Op::*;
    use std::io::Write;

    let mut data = Vec::new();
    let mut current_point = None;
    let f = &mut data;

    while ops.len() > 0 {
        let mut advance = 1;
        match ops[0] {
            BeginMarkedContent { ref tag, properties: Some(ref name) } => {
                serialize_name(&tag, f)?;
                write!(f, " ")?;
                name.serialize(f, 0)?;
                writeln!(f, " BDC")?;
            }
            BeginMarkedContent { ref tag, properties: None } => {
                serialize_name(&tag, f)?;
                writeln!(f, " BMC")?;
            }
            MarkedContentPoint { ref tag, properties: Some(ref name) } => {
                serialize_name(&tag, f)?;
                write!(f, " ")?;
                name.serialize(f, 0)?;
                writeln!(f, " DP")?;
            }
            MarkedContentPoint { ref tag, properties: None } => {
                serialize_name(&tag, f)?;
                writeln!(f, " MP")?;
            }
            EndMarkedContent => writeln!(f, "EMC")?,
            Close => match ops.get(1) {
                Some(Stroke) => {
                    writeln!(f, "s")?;
                    advance += 1;
                }
                Some(FillAndStroke { winding: Winding::NonZero }) => {
                    writeln!(f, "b")?;
                    advance += 1;
                }
                Some(FillAndStroke { winding: Winding::EvenOdd }) => {
                    writeln!(f, "b*")?;
                    advance += 1;
                }
                _ => writeln!(f, "h")?,
            }
            MoveTo { p } => {
                writeln!(f, "{} m", p)?;
                current_point = Some(p);
            }
            LineTo { p } => {
                writeln!(f, "{} l", p)?;
                current_point = Some(p);
            },
            CurveTo { c1, c2, p } => {
                if Some(c1) == current_point {
                    writeln!(f, "{} {} v", c2, p)?;
                } else if c2 == p {
                    writeln!(f, "{} {} y", c1, p)?;
                } else {
                    writeln!(f, "{} {} {} y", c1, c2, p)?;
                }
                current_point = Some(p);
            },
            Rect { rect } => writeln!(f, "{} re", rect)?,
            EndPath => writeln!(f, "n")?,
            Stroke => writeln!(f, "S")?,
            FillAndStroke { winding: Winding::NonZero } => writeln!(f, "B")?,
            FillAndStroke { winding: Winding::EvenOdd } => writeln!(f, "B*")?,
            Fill { winding: Winding::NonZero } => writeln!(f, "f")?,
            Fill { winding: Winding::EvenOdd } => writeln!(f, "f*")?,
            Shade { ref name } => {
                serialize_name(name, f)?;
                writeln!(f, " sh")?;
            },
            Clip { winding: Winding::NonZero } => writeln!(f, "W")?,
            Clip { winding: Winding::EvenOdd } => writeln!(f, "W*")?,
            Save => writeln!(f, "q")?,
            Restore => writeln!(f, "Q")?,
            Transform { matrix } => writeln!(f, "{} cm", matrix)?,
            LineWidth { width } => writeln!(f, "{} w", width)?,
            Dash { ref pattern, phase } => write!(f, "[{}] {} d", pattern.iter().format(" "), phase)?,
            LineJoin { join } => writeln!(f, "{} j", join as u8)?,
            LineCap { cap } => writeln!(f, "{} J", cap as u8)?,
            MiterLimit { limit } => writeln!(f, "{} M", limit)?,
            Flatness { tolerance } => writeln!(f, "{} i", tolerance)?,
            GraphicsState { ref name } => {
                serialize_name(name, f)?;
                writeln!(f, " gs")?;
            },
            StrokeColor { color: Color::Gray(g) } => writeln!(f, "{} G", g)?,
            StrokeColor { color: Color::Rgb(rgb) } => writeln!(f, "{} RG", rgb)?,
            StrokeColor { color: Color::Cmyk(cmyk) } => writeln!(f, "{} K", cmyk)?,
            StrokeColor { color: Color::Other(ref args) } =>  {
                for p in args {
                    p.serialize(f, 0)?;
                    write!(f, " ")?;
                }
                writeln!(f, "SCN")?;
            }
            FillColor { color: Color::Gray(g) } => writeln!(f, "{} g", g)?,
            FillColor { color: Color::Rgb(rgb) } => writeln!(f, "{} rg", rgb)?,
            FillColor { color: Color::Cmyk(cmyk) } => writeln!(f, "{} k", cmyk)?,
            FillColor { color: Color::Other(ref args) } => {
                for p in args {
                    p.serialize(f, 0)?;
                    write!(f, " ")?;
                }
                writeln!(f, "scn")?;
            }
            FillColorSpace { ref name } => {
                serialize_name(name, f)?;
                writeln!(f, " cs")?;
            },
            StrokeColorSpace { ref name } => {
                serialize_name(name, f)?;
                writeln!(f, " CS")?;
            },

            RenderingIntent { intent } => writeln!(f, "{} ri", intent.to_str())?,
            Op::BeginText => writeln!(f, "BT")?,
            Op::EndText => writeln!(f, "ET")?,
            CharSpacing { char_space } => writeln!(f, "{} Tc", char_space)?,
            WordSpacing { word_space } => {
                if let [
                    Op::CharSpacing { char_space },
                    Op::TextNewline,
                    Op::TextDraw { ref text },
                    ..
                ] = ops[1..] {
                    write!(f, "{} {} ", word_space, char_space)?;
                    text.serialize(f)?;
                    writeln!(f, " \"")?;
                    advance += 3;
                } else {
                    writeln!(f, "{} Tw", word_space)?;
                }
            }
            TextScaling { horiz_scale } => writeln!(f, "{} Tz", horiz_scale)?,
            Leading { leading } => match ops[1..] {
                [Op::MoveTextPosition { translation }, ..] if leading == -translation.x => {
                    writeln!(f, "{} {} TD", translation.x, translation.y)?;
                    advance += 1;
                }
                _ => {
                    writeln!(f, "{} TL", leading)?;
                }
            }
            TextFont { ref name, ref size } => {
                serialize_name(name, f)?;
                writeln!(f, " {} Tf", size)?;
            },
            TextRenderMode { mode } => writeln!(f, "{} Tr", mode as u8)?,
            TextRise { rise } => writeln!(f, "{} Ts", rise)?,
            MoveTextPosition { translation } => writeln!(f, "{} {} Td", translation.x, translation.y)?,
            SetTextMatrix { matrix } => writeln!(f, "{} Tm", matrix)?,
            TextNewline => {
                if let [Op::TextDraw { ref text }, ..] = ops[1..] {
                    text.serialize(f)?;
                    writeln!(f, " '")?;
                    advance += 1;
                } else {
                    writeln!(f, "T*")?;
                }
            },
            TextDraw { ref text } => {
                text.serialize(f)?;
                writeln!(f, " Tj")?;
            },
            TextDrawAdjusted { ref array } => {
                writeln!(f, "[{}] TJ", array.iter().format(" "))?;
            },
            InlineImage { ref image } => unimplemented!(),
            XObject { ref name } => {
                serialize_name(name, f)?;
                writeln!(f, " Do")?;
            },
        }
        ops = &ops[advance..];
    }
    Ok(data)
}

impl Content {
    pub fn from_ops(operations: Vec<Op>) -> Self {
        let data = serialize_ops(&operations).unwrap();
        Content {
            operations,
            parts: vec![Stream::new((), data)]
        }
    }
}

impl ObjectWrite for Content {
    fn to_primitive(&self, update: &mut impl Updater) -> Result<Primitive> {
        if self.parts.len() == 1 {
            self.parts[0].to_primitive(update)
        } else {
            self.parts.to_primitive(update)
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Winding {
    EvenOdd,
    NonZero
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum LineCap {
    Butt = 0,
    Round = 1,
    Square = 2,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum LineJoin {
    Miter = 0,
    Round = 1,
    Bevel = 2,
}


#[derive(Debug, Copy, Clone, PartialEq, Default)]
#[repr(C, align(8))]
pub struct Point {
    pub x: f32,
    pub y: f32
}
impl Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {}", self.x, self.y)
    }
}
impl Mul<Matrix> for Point {
    type Output = Matrix;

    fn mul(self, rhs: Matrix) -> Self::Output {
        Matrix {
            e: self.x * rhs.a + self.y * rhs.c + rhs.e,
            f: self.x * rhs.b + self.y * rhs.d + rhs.f,
            ..rhs
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
#[repr(C, align(8))]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {} {} {}", self.x, self.y, self.width, self.height)
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
#[repr(C, align(8))]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}
impl Display for Matrix {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {} {} {} {} {}", self.a, self.b, self.c, self.d, self.e, self.f)
    }
}
impl Default for Matrix {
    fn default() -> Self {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}
impl Mul for Matrix {
    type Output = Matrix;

    fn mul(self, rhs: Matrix) -> Self::Output {
        Matrix {
            a: self.a * rhs.a + self.b * rhs.c,
            b: self.a * rhs.b + self.b * rhs.d,
            c: self.c * rhs.a + self.d * rhs.c,
            d: self.c * rhs.b + self.d * rhs.d,
            e: self.e * rhs.a + self.f * rhs.c + rhs.e,
            f: self.e * rhs.b + self.f * rhs.d + rhs.f,
        }
    }
}
impl Mul<Point> for Matrix {
    type Output = Matrix;

    fn mul(self, rhs: Point) -> Self::Output {
        Matrix {
            e: self.e + rhs.x,
            f: self.f + rhs.y,
            ..self
        }
    }
}

#[derive(Debug, Clone)]
pub enum Color {
    Gray(f32),
    Rgb(Rgb),
    Cmyk(Cmyk),
    Other(Vec<Primitive>),
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TextMode {
    Fill,
    Stroke,
    FillThenStroke,
    Invisible,
    FillAndClip,
    StrokeAndClip
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Rgb {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}
impl Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {} {}", self.red, self.green, self.blue)
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Cmyk {
    pub cyan: f32,
    pub magenta: f32,
    pub yellow: f32,
    pub key: f32,
}
impl Display for Cmyk {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {} {} {}", self.cyan, self.magenta, self.yellow, self.key)
    }
}


/// Graphics Operator
/// 
/// See PDF32000 A.2
#[derive(Debug, Clone)]
pub enum Op {
    /// Begin a marked comtent sequence
    /// 
    /// Pairs with the following EndMarkedContent.
    /// 
    /// generated by operators `BMC` and `BDC`
    BeginMarkedContent { tag: String, properties: Option<Primitive> },

    /// End a marked content sequence.
    /// 
    /// Pairs with the previous BeginMarkedContent.
    /// 
    /// generated by operator `EMC`
    EndMarkedContent,

    /// A marked content point.
    /// 
    /// generated by operators `MP` and `DP`.
    MarkedContentPoint { tag: String, properties: Option<Primitive> },


    Close,
    MoveTo { p: Point },
    LineTo { p: Point },
    CurveTo { c1: Point, c2: Point, p: Point },
    Rect { rect: Rect },
    EndPath,

    Stroke,

    /// Fill and Stroke operation
    /// 
    /// generated by operators `b`, `B`, `b*`, `B*`
    /// `close` indicates whether the path should be closed first
    FillAndStroke { winding: Winding },


    Fill { winding: Winding },

    /// Fill using the named shading pattern
    /// 
    /// operator: `sh`
    Shade { name: String },

    Clip { winding: Winding },

    Save,
    Restore,

    Transform { matrix: Matrix },

    LineWidth { width: f32 },
    Dash { pattern: Vec<f32>, phase: f32 },
    LineJoin { join: LineJoin },
    LineCap { cap: LineCap },
    MiterLimit { limit: f32 },
    Flatness { tolerance: f32 },

    GraphicsState { name: String },

    StrokeColor { color: Color },
    FillColor { color: Color },

    FillColorSpace { name: String },
    StrokeColorSpace { name: String },

    RenderingIntent { intent: RenderingIntent },

    BeginText,
    EndText,

    CharSpacing { char_space: f32 },
    WordSpacing { word_space: f32 },
    TextScaling { horiz_scale: f32 },
    Leading { leading: f32 },
    TextFont { name: String, size: f32 },
    TextRenderMode { mode: TextMode },

    /// `Ts`
    TextRise { rise: f32 },

    /// `Td`, `TD`
    MoveTextPosition { translation: Point },

    /// `Tm`
    SetTextMatrix { matrix: Matrix },

    /// `T*`
    TextNewline,

    /// `Tj`
    TextDraw { text: PdfString },

    TextDrawAdjusted { array: Vec<Primitive> },

    XObject { name: String },

    InlineImage { image: Stream::<ImageDict> },
}