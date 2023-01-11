//! The general strategy is just to use a plist for storage. Also, lots of
//! unwrapping.
//!
//! There are lots of other ways this could go, including something serde-like
//! where it gets serialized to more Rust-native structures, proc macros, etc.

use std::collections::{BTreeMap, HashSet};
use std::hash::Hash;
use std::{fs, path};

use log::warn;
use ordered_float::OrderedFloat;
use regex::Regex;

use crate::error::Error;
use crate::from_plist::FromPlist;
use crate::plist::Plist;

const V3_METRIC_NAMES: [&str; 5] = [
    "ascender",
    "baseline",
    "descender",
    "cap height",
    "x-height",
];
const V2_METRIC_NAMES: [&str; 5] = ["ascender", "baseline", "descender", "capHeight", "xHeight"];

type RawUserToDesignMapping = Vec<(OrderedFloat<f32>, OrderedFloat<f32>)>;

/// A tidied up font from a plist.
///
/// Normalized representation of Glyphs 2/3 content
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Font {
    pub family_name: String,
    pub axes: Vec<Axis>,
    pub font_master: Vec<FontMaster>,
    pub default_master_idx: usize,
    pub glyphs: BTreeMap<String, Glyph>,
    pub glyph_order: Vec<String>,
    pub codepoints: BTreeMap<Vec<u32>, String>,
    // tag => (user:design) tuples
    pub axis_mappings: BTreeMap<String, RawUserToDesignMapping>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Glyph {
    pub glyphname: String,
    pub layers: Vec<Layer>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Layer {
    pub layer_id: String,
    pub width: OrderedFloat<f64>,
    pub shapes: Vec<Shape>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    Path(Path),
    Component(Component),
}

// The font you get directly from a plist, minimally modified
// Types chosen specifically to accomodate plist translation.
#[derive(Debug, FromPlist, PartialEq, Eq)]
struct RawFont {
    pub family_name: String,
    pub axes: Option<Vec<Axis>>,
    pub glyphs: Vec<RawGlyph>,
    pub font_master: Vec<FontMaster>,
    #[rest]
    pub other_stuff: BTreeMap<String, Plist>,
}

#[derive(Clone, Debug, FromPlist, PartialEq, Eq, Hash)]
pub struct Axis {
    pub name: String,
    pub tag: String,
    pub hidden: Option<bool>,
}

#[derive(Clone, Debug, FromPlist, PartialEq, Eq, Hash)]
pub struct RawGlyph {
    pub layers: Vec<RawLayer>,
    pub glyphname: String,
    #[rest]
    pub other_stuff: BTreeMap<String, Plist>,
}

#[derive(Clone, Debug, FromPlist, PartialEq, Eq, Hash)]
pub struct RawLayer {
    pub layer_id: String,
    pub associated_master_id: Option<String>,
    pub width: OrderedFloat<f64>,
    shapes: Option<Vec<RawShape>>,
    paths: Option<Vec<Path>>,
    components: Option<Vec<Component>>,
    //pub anchors: Option<Vec<Anchor>>,
    #[rest]
    pub other_stuff: BTreeMap<String, Plist>,
}

/// Represents a path OR a component
///
/// <https://github.com/schriftgestalt/GlyphsSDK/blob/Glyphs3/GlyphsFileFormat/GlyphsFileFormatv3.md#differences-between-version-2>
#[derive(Clone, Debug, FromPlist, PartialEq, Eq, Hash)]
pub struct RawShape {
    // TODO: add numerous unsupported attributes

    // When I'm a path
    pub closed: Option<bool>,
    pub nodes: Option<Vec<Node>>,

    // When I'm a component I specifically want all my attributes to end up in other_stuff
    // My Component'ness can be detected by presence of a ref (Glyphs3) or name(Glyphs2) attribute

    // Always
    #[rest]
    pub other_stuff: BTreeMap<String, Plist>,
}

#[derive(Clone, Debug, FromPlist, PartialEq, Eq, Hash)]
pub struct Path {
    pub closed: bool,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Component {
    /// The glyph this component references
    pub glyph_name: String,
    pub transform: Affine,
    pub other_stuff: BTreeMap<String, Plist>,
}

// We do not use kurbo's point because it does not hash
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Point {
    x: OrderedFloat<f64>,
    y: OrderedFloat<f64>,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Point {
        Point {
            x: x.into(),
            y: y.into(),
        }
    }
}

// We do not use kurbo's affine because it does not hash
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Affine([OrderedFloat<f64>; 6]);

impl Affine {
    pub fn new(matrix: [f64; 6]) -> Affine {
        Affine([
            matrix[0].into(),
            matrix[1].into(),
            matrix[2].into(),
            matrix[3].into(),
            matrix[4].into(),
            matrix[5].into(),
        ])
    }

    pub fn identity() -> Affine {
        Affine([
            1f64.into(),
            0f64.into(),
            0f64.into(),
            1f64.into(),
            0f64.into(),
            0f64.into(),
        ])
    }

    pub fn translate(x: f64, y: f64) -> Affine {
        Affine([
            1f64.into(),
            0f64.into(),
            0f64.into(),
            1f64.into(),
            x.into(),
            y.into(),
        ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Node {
    pub pt: Point,
    pub node_type: NodeType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeType {
    Line,
    LineSmooth,
    OffCurve,
    Curve,
    CurveSmooth,
}

#[derive(Clone, Debug, FromPlist, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub name: String,
    pub position: Point,
}

#[derive(Debug, Clone, FromPlist, PartialEq, Eq, Hash)]
pub struct FontMaster {
    pub id: String,
    pub axes_values: Option<Vec<OrderedFloat<f64>>>,
    #[rest]
    pub other_stuff: BTreeMap<String, Plist>,
}

impl RawGlyph {
    pub fn get_layer(&self, layer_id: &str) -> Option<&RawLayer> {
        self.layers.iter().find(|l| l.layer_id == layer_id)
    }
}

impl FromPlist for Node {
    fn from_plist(plist: Plist) -> Self {
        match &plist {
            Plist::String(value) => {
                let mut spl = value.splitn(3, ' ');
                let x = spl.next().unwrap().parse().unwrap();
                let y = spl.next().unwrap().parse().unwrap();
                let pt = Point::new(x, y);
                let node_type = spl.next().unwrap().parse().unwrap();
                Node { pt, node_type }
            }
            Plist::Array(value) => {
                if value.len() != 3 {
                    panic!("Invalid node content {:?}", plist);
                };
                let x = value[0].as_f64().unwrap();
                let y = value[1].as_f64().unwrap();
                let pt = Point::new(x, y);
                let node_type = value[2].as_str().unwrap().parse().unwrap();
                Node { pt, node_type }
            }
            _ => {
                panic!("Invalid node content {:?}", plist);
            }
        }
    }
}

impl std::str::FromStr for NodeType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            // Glyphs 2 style
            "LINE" => Ok(NodeType::Line),
            "LINE SMOOTH" => Ok(NodeType::LineSmooth),
            "OFFCURVE" => Ok(NodeType::OffCurve),
            "CURVE" => Ok(NodeType::Curve),
            "CURVE SMOOTH" => Ok(NodeType::CurveSmooth),
            // Glyphs 3 style
            "l" => Ok(NodeType::Line),
            "ls" => Ok(NodeType::LineSmooth),
            "o" => Ok(NodeType::OffCurve),
            "c" => Ok(NodeType::Curve),
            "cs" => Ok(NodeType::CurveSmooth),
            _ => Err(format!("unknown node type {}", s)),
        }
    }
}

fn try_f64(plist: &Plist) -> Result<f64, Error> {
    plist
        .as_f64()
        .ok_or_else(|| Error::StructuralError(format!("Bad f64:{:?}", plist)))
}

impl TryFrom<BTreeMap<String, Plist>> for Component {
    type Error = Error;

    fn try_from(mut dict: BTreeMap<String, Plist>) -> Result<Self, Self::Error> {
        // Glyphs v3 name has been renamed to ref; look for both
        let glyph_name = if let Some(Plist::String(glyph_name)) = dict.remove("ref") {
            glyph_name
        } else if let Some(Plist::String(glyph_name)) = dict.remove("name") {
            glyph_name
        } else {
            return Err(Error::StructuralError(format!(
                "Neither ref nor name present: {:?}",
                dict
            )));
        };

        // V3 vs v2: The transform entry has been replaced by angle, pos and scale entries.
        let mut transform = if let Some(plist) = dict.remove("transform") {
            Affine::from_plist(plist)
        } else {
            Affine::identity()
        };

        if let Some(Plist::Array(pos)) = dict.remove("pos") {
            assert!(transform == Affine::identity());
            if pos.len() != 2 {
                return Err(Error::StructuralError(format!("Bad pos: {:?}", pos)));
            }
            transform = Affine::translate(try_f64(&pos[0])?, try_f64(&pos[1])?);
        }
        // TODO scale, angle, etc
        // See component in <https://github.com/schriftgestalt/GlyphsSDK/blob/Glyphs3/GlyphsFileFormat/GlyphsFileFormatv3.md#differences-between-version-2>
        if let Some(..) = dict.remove("angle") {
            panic!("Angle not supported yet");
        }
        if let Some(..) = dict.remove("scale") {
            panic!("Scale not supported yet");
        }

        Ok(Component {
            glyph_name,
            transform,
            other_stuff: dict,
        })
    }
}

impl FromPlist for Component {
    fn from_plist(plist: Plist) -> Self {
        let Plist::Dictionary(dict) = plist else {
            panic!("Component must be a dict: {:?}", plist);
        };
        dict.try_into().expect("Unable to parse Component")
    }
}

impl FromPlist for Affine {
    fn from_plist(plist: Plist) -> Self {
        let raw = plist.as_str().unwrap();
        let raw = &raw[1..raw.len() - 1];
        let coords: Vec<f64> = raw.split(", ").map(|c| c.parse().unwrap()).collect();
        Affine::new([
            coords[0], coords[1], coords[2], coords[3], coords[4], coords[5],
        ])
    }
}

impl FromPlist for Point {
    fn from_plist(plist: Plist) -> Self {
        let raw = plist.as_str().unwrap();
        let raw = &raw[1..raw.len() - 1];
        let coords: Vec<f64> = raw.split(", ").map(|c| c.parse().unwrap()).collect();
        Point::new(coords[0], coords[1])
    }
}

impl FromPlist for OrderedFloat<f64> {
    fn from_plist(plist: Plist) -> Self {
        plist.as_f64().unwrap().into()
    }
}

impl Path {
    pub fn new(closed: bool) -> Path {
        Path {
            nodes: Vec::new(),
            closed,
        }
    }

    pub fn add(&mut self, pt: impl Into<Point>, node_type: NodeType) {
        let pt = pt.into();
        self.nodes.push(Node { pt, node_type });
    }

    /// Rotate left by one, placing the first point at the end. This is because
    /// it's what glyphs seems to expect.
    pub fn rotate_left(&mut self, delta: usize) {
        self.nodes.rotate_left(delta);
    }

    pub fn reverse(&mut self) {
        self.nodes.reverse();
    }
}

fn for_raw_glyphs(
    glyphs_file: &path::Path,
    root_dict: &mut BTreeMap<String, Plist>,
    callback: fn(&mut BTreeMap<String, Plist>) -> Result<(), Error>,
) -> Result<(), Error> {
    if !root_dict.contains_key("glyphs") {
        return Ok(());
    }
    let Plist::Array(glyphs) = root_dict.get_mut("glyphs").unwrap() else {
        return Err(Error::ParseError(glyphs_file.to_path_buf(), "Must have a glyphs array".to_string()));
    };
    for glyph in glyphs.iter_mut() {
        let Plist::Dictionary(glyph) = glyph else {
            return Err(Error::ParseError(glyphs_file.to_path_buf(), "Glyph must be a dict".to_string()));
        };
        callback(glyph)?;
    }
    Ok(())
}

fn fix_glyphs_named_infinity(
    glyphs_file: &path::Path,
    root_dict: &mut BTreeMap<String, Plist>,
) -> Result<(), Error> {
    for_raw_glyphs(glyphs_file, root_dict, |glyph| {
        if !glyph.contains_key("glyphname") {
            return Ok(());
        }
        if let Plist::Float(..) = glyph.get("glyphname").unwrap() {
            glyph.insert(
                "glyphname".to_string(),
                Plist::String("infinity".to_string()),
            );
        }
        Ok(())
    })
}

fn custom_params_mut(other_stuff: &mut BTreeMap<String, Plist>) -> Option<&mut Vec<Plist>> {
    let custom_params = other_stuff.get_mut("customParameters");
    custom_params.as_ref()?;
    let Some(Plist::Array(custom_params)) = custom_params else {
        warn!("customParameters isn't an array\n{:#?}", custom_params);
        return None;
    };
    Some(custom_params)
}

fn custom_param_mut<'a>(
    other_stuff: &'a mut BTreeMap<String, Plist>,
    key: &str,
) -> Option<(usize, &'a mut Plist)> {
    let Some(custom_params) = custom_params_mut(other_stuff) else {
        return None;
    };

    let name_key = "name".to_string();
    for (idx, custom_param) in custom_params.iter_mut().enumerate() {
        let Plist::Dictionary(dict) = custom_param else {
            warn!("custom param isn't a dictionary\n{:#?}", custom_param);
            continue;
        };
        let Some(Plist::String(param_key)) = dict.get(&name_key) else {
            warn!("custom param has a non-string name\n{:#?}", custom_param);
            continue;
        };
        if key == param_key {
            return Some((idx, custom_param));
        }
    }
    None
}

fn glyphs_v2_field_name_and_default(nth_axis: usize) -> Result<(&'static str, f64), Error> {
    // Per https://github.com/googlefonts/fontmake-rs/pull/42#pullrequestreview-1211619812
    // the field to use is based on the order in axes NOT the tag.
    // That is, whatever the first axis is, it's value is in the weightValue field. Long sigh.
    // Defaults per https://github.com/googlefonts/fontmake-rs/pull/42#discussion_r1044415236.
    Ok(match nth_axis {
        0 => ("weightValue", 100_f64),
        1 => ("widthValue", 100_f64),
        2 => ("customValue", 0_f64),
        _ => {
            return Err(Error::StructuralError(format!(
                "We don't know what field to use for axis {}",
                nth_axis
            )))
        }
    })
}

fn custom_params(other_stuff: &BTreeMap<String, Plist>) -> Option<&Vec<Plist>> {
    let custom_params = other_stuff.get("customParameters");
    custom_params.as_ref()?;
    let Some(Plist::Array(custom_params)) = custom_params else {
        warn!("customParameters isn't an array\n{:#?}", custom_params);
        return None;
    };
    Some(custom_params)
}

fn custom_param<'a>(
    other_stuff: &'a BTreeMap<String, Plist>,
    key: &str,
) -> Option<(usize, &'a Plist)> {
    let Some(custom_params) = custom_params(other_stuff) else {
        return None;
    };

    let name_key = "name".to_string();
    for (idx, custom_param) in custom_params.iter().enumerate() {
        let Plist::Dictionary(dict) = custom_param else {
            warn!("custom param isn't a dictionary\n{:#?}", custom_param);
            continue;
        };
        let Some(Plist::String(param_key)) = dict.get(&name_key) else {
            warn!("custom param has a non-string name\n{:#?}", custom_param);
            continue;
        };
        if key == param_key {
            return Some((idx, custom_param));
        }
    }
    None
}

impl RawFont {
    fn is_v2(&self) -> bool {
        let mut is_v2 = true;
        if let Some(Plist::Integer(version)) = self.other_stuff.get(".formatVersion") {
            is_v2 = *version < 3; // .formatVersion is only present for v3+
        }
        is_v2
    }

    fn v2_to_v3_axes(&mut self) -> Result<Vec<String>, Error> {
        let mut tags = Vec::new();
        if self.axes.is_none() {
            self.axes = Some(Vec::new());
        }
        if let Some((axes_idx, custom_param)) = custom_param_mut(&mut self.other_stuff, "Axes") {
            if let Plist::Dictionary(dict) = custom_param {
                let v3_axes = self.axes.as_mut().unwrap();
                let Some(Plist::Array(v2_axes)) = dict.get_mut("value") else {
                    return Err(Error::StructuralError("No value for Axes custom parameter".into()));
                };
                for v2_axis in v2_axes {
                    let Plist::Dictionary(v2_axis) = v2_axis else {
                        return Err(Error::StructuralError("Axis value must be a dict".into()));
                    };
                    let tag = v2_axis.get("Tag").unwrap().as_str().unwrap();
                    tags.push(tag.to_string());
                    v3_axes.push(Axis {
                        name: v2_axis.get("Name").unwrap().as_str().unwrap().into(),
                        tag: tag.into(),
                        hidden: v2_axis.get("hidden").map(|v| v.as_i64() == Some(1)),
                    });
                }
            }
            custom_params_mut(&mut self.other_stuff).map(|p| p.remove(axes_idx));
        }

        // Match the defaults from https://github.com/googlefonts/glyphsLib/blob/f6e9c4a29ce764d34c309caef5118c48c156be36/Lib/glyphsLib/builder/axes.py#L526
        // if we have nothing
        let axes = self.axes.as_mut().unwrap();
        if axes.is_empty() {
            axes.push(Axis {
                name: "Weight".into(),
                tag: "wght".into(),
                hidden: None,
            });
            axes.push(Axis {
                name: "Width".into(),
                tag: "wdth".into(),
                hidden: None,
            });
            axes.push(Axis {
                name: "Custom".into(),
                tag: "XXXX".into(),
                hidden: None,
            });
        }

        if axes.len() > 3 {
            return Err(Error::StructuralError(
                "We only understand 0..3 axes for Glyphs v2".into(),
            ));
        }

        // v2 stores values for axes in specific fields, find them and put them into place
        // "Axis position related properties (e.g. weightValue, widthValue, customValue) have been replaced by the axesValues list which is indexed in parallel with the toplevel axes list."
        for master in self.font_master.iter_mut() {
            let mut axis_values = Vec::new();
            for idx in 0..axes.len() {
                // Per https://github.com/googlefonts/fontmake-rs/pull/42#pullrequestreview-1211619812
                // the field to use is based on the order in axes NOT the tag.
                // That is, whatever the first axis is, it's value is in the weightValue field. Long sigh.
                let (field_name, default_value) = glyphs_v2_field_name_and_default(idx)?;
                let value = master
                    .other_stuff
                    .remove(field_name)
                    .unwrap_or_else(|| Plist::Float(default_value.into()))
                    .as_f64()
                    .ok_or_else(|| {
                        Error::StructuralError(format!(
                            "Invalid '{}' in\n{:#?}",
                            field_name, master.other_stuff
                        ))
                    })?;
                axis_values.push(value.into());
            }
            master.axes_values = Some(axis_values);
        }

        if custom_params_mut(&mut self.other_stuff).map_or(false, |d| d.is_empty()) {
            self.other_stuff.remove("customParameters");
        }
        Ok(tags)
    }

    fn v2_to_v3_metrics(&mut self) -> Result<(), Error> {
        // metrics are in parallel arrays in v3
        assert!(V2_METRIC_NAMES.len() == V3_METRIC_NAMES.len());

        // setup root storage for the basic metrics
        let mut metrics = Vec::new();
        for name in V3_METRIC_NAMES {
            metrics.push(Plist::Dictionary(BTreeMap::from([(
                "type".to_string(),
                Plist::String(name.into()),
            )])));
        }
        self.other_stuff
            .insert("metrics".into(), Plist::Array(metrics));

        // in each font master setup the parallel array
        for master in self.font_master.iter_mut() {
            let mut metric_dicts = Vec::new();
            for v2_name in V2_METRIC_NAMES.iter() {
                let mut dict = BTreeMap::new();
                if let Some(Plist::Integer(value)) = master.other_stuff.remove(&v2_name.to_string())
                {
                    // leave blank for 0
                    if value != 0 {
                        dict.insert("pos".to_string(), Plist::Integer(value));
                    }
                }
                metric_dicts.push(dict);
            }
            // "alignmentZones is now a set of over (overshoot) properties attached to metrics"
            if let Some(Plist::Array(zones)) = master.other_stuff.get("alignmentZones") {
                for (idx, zone) in zones.iter().enumerate() {
                    let Plist::String(zone) = zone else {
                        warn!("Non-string alignment zone, skipping");
                        continue;
                    };
                    // Alignment zones look like {800, 16}, but (800, 16) would be more useful
                    let zone = zone.replace('{', "(").replace('}', ")");
                    let Ok(Plist::Array(values)) = Plist::parse(&zone) else {
                        warn!("Confusing alignment zone, skipping");
                        continue;
                    };
                    if values.len() != 2 {
                        warn!("Confusing alignment zone, skipping");
                        continue;
                    };
                    // values are pos, over. pos comes across from metrics so just copy non-zero over's.
                    let Plist::Integer(over) = values[1] else {
                        warn!("Confusing alignment zone, skipping");
                        continue;
                    };
                    if over != 0 {
                        metric_dicts[idx].insert("over".into(), Plist::Integer(over));
                    }
                }
            }
            master.other_stuff.remove("alignmentZones");

            let mut metric_plists = Vec::new();
            for metric_dict in metric_dicts {
                metric_plists.push(Plist::Dictionary(metric_dict));
            }
            master
                .other_stuff
                .insert("metricValues".into(), Plist::Array(metric_plists));
        }
        Ok(())
    }

    fn v2_to_v3_weight(&mut self) -> Result<(), Error> {
        for master in self.font_master.iter_mut() {
            // Don't remove weightValue, we need it to understand axes
            let Some(Plist::Integer(..)) = master.other_stuff.get("weightValue") else {
                continue;
            };
            let name = match master.other_stuff.remove("weight") {
                Some(Plist::String(name)) => name,
                _ => String::from("Regular"), // Missing = default = Regular per @anthrotype
            };
            master
                .other_stuff
                .insert("name".into(), Plist::String(name));
        }
        Ok(())
    }

    /// `<See https://github.com/schriftgestalt/GlyphsSDK/blob/Glyphs3/GlyphsFileFormat/GlyphsFileFormatv3.md#differences-between-version-2>`
    fn v2_to_v3(&mut self) -> Result<(), Error> {
        self.v2_to_v3_weight()?;
        self.v2_to_v3_axes()?;
        self.v2_to_v3_metrics()?;
        Ok(())
    }
}

fn parse_glyph_order(raw_font: &RawFont) -> Vec<String> {
    let mut valid_names: HashSet<_> = raw_font.glyphs.iter().map(|g| &g.glyphname).collect();
    let mut glyph_order = Vec::new();

    // Add all valid glyphOrder entries in order
    // See https://github.com/googlefonts/fontmake-rs/pull/43/files#r1044627972
    if let Some((_, Plist::Dictionary(dict))) = custom_param(&raw_font.other_stuff, "glyphOrder") {
        if let Some(Plist::Array(entries)) = dict.get("value") {
            entries
                .iter()
                .filter_map(|e| e.as_str())
                .map(|s| s.to_string())
                .for_each(|s| {
                    if valid_names.remove(&s) {
                        glyph_order.push(s);
                    }
                });
        };
    };

    // Add anything left over in file order
    raw_font
        .glyphs
        .iter()
        .filter(|g| valid_names.contains(&g.glyphname))
        .for_each(|g| glyph_order.push(g.glyphname.clone()));

    glyph_order
}

fn parse_codepoints(raw_font: &mut RawFont, radix: u32) -> BTreeMap<Vec<u32>, String> {
    let mut cp_to_name = BTreeMap::new();
    for glyph in raw_font.glyphs.iter_mut() {
        if let Some(Plist::String(val)) = glyph.other_stuff.remove("unicode") {
            let codepoints = val
                .split(',')
                .into_iter()
                .map(|v| i64::from_str_radix(v, radix).unwrap() as u32)
                .collect();
            cp_to_name.insert(codepoints, glyph.glyphname.clone());
        };
    }
    cp_to_name
}

fn default_master_idx(raw_font: &RawFont) -> usize {
    // https://github.com/googlefonts/fontmake-rs/issues/44
    custom_param(&raw_font.other_stuff, "Variable Font Origin")
        .map(|(_, param)| {
            let Plist::Dictionary(dict) = param else {
                return 0;
            };
            let Some(Plist::String(origin)) = dict.get("value") else {
                warn!("Incomprehensible Variable Font Origin");
                return 0;
            };
            raw_font
                .font_master
                .iter()
                .enumerate()
                .find(|(_, master)| &master.id == origin)
                .map(|(idx, _)| idx)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn axis_index(from: &RawFont, pred: impl Fn(&Axis) -> bool) -> Option<usize> {
    from.axes
        .as_ref()
        .map(|axes| {
            axes.iter()
                .enumerate()
                .find_map(|(i, a)| if pred(a) { Some(i) } else { None })
        })
        .unwrap_or_default()
}

fn user_to_design_from_axis_mapping(
    from: &RawFont,
) -> Option<BTreeMap<String, RawUserToDesignMapping>> {
    // Fetch mapping from Axis Mappings, if any
    let Some((_, axis_map)) = custom_param(&from.other_stuff, "Axis Mappings") else {
        return None;
    };
    let Plist::Dictionary(axis_map) = axis_map.get("value").unwrap() else {
        panic!("Incomprehensible axis map {:?}", axis_map);
    };
    let mut axis_mappings: BTreeMap<String, RawUserToDesignMapping> = BTreeMap::new();
    for (axis_tag, mappings) in axis_map.iter() {
        let Plist::Dictionary(mappings) = mappings else {
            panic!("Incomprehensible mappings {:?}", mappings);
        };
        let Some(axis_index) = axis_index(from, |a| &a.tag == axis_tag) else {
            panic!("No such axes: {:?}", axis_tag);
        };
        // We could not have found an axis index if there are no axes
        let axis_name = &from.axes.as_ref().unwrap().get(axis_index).unwrap().name;
        for (user, design) in mappings.iter() {
            let user: f32 = user.parse().unwrap();
            let design = design.as_f64().unwrap() as f32;
            axis_mappings
                .entry(axis_name.clone())
                .or_default()
                .push((user.into(), design.into()));
        }
    }
    Some(axis_mappings)
}

fn user_to_design_from_axis_location(
    from: &RawFont,
) -> Option<BTreeMap<String, RawUserToDesignMapping>> {
    // glyphsLib only trusts Axis Location when all masters have it, match that
    // https://github.com/googlefonts/fontmake-rs/pull/83#discussion_r1065814670
    let master_locations: Vec<&Plist> = from
        .font_master
        .iter()
        .filter_map(|m| custom_param(&m.other_stuff, "Axis Location").map(|(_, al)| al))
        .collect();
    if master_locations.len() != from.font_master.len() {
        if !master_locations.is_empty() {
            warn!(
                "{}/{} masters have Axis Location; ignoring",
                master_locations.len(),
                from.font_master.len()
            );
        }
        return None;
    }

    let mut axis_mappings: BTreeMap<String, RawUserToDesignMapping> = BTreeMap::new();
    for (master, axis_locations) in from.font_master.iter().zip(&master_locations) {
        let Plist::Dictionary(axis_locations) = axis_locations else {
            panic!("Axis Location must be a dict {:?}", axis_locations);
        };
        let Some(Plist::Array(axis_locations)) = axis_locations.get("value") else {
            panic!("Value must be a dict {:?}", axis_locations);
        };
        for axis_location in axis_locations {
            let Some(Plist::String(axis_name)) = axis_location.get("Axis") else {
                panic!("Axis name must be a string {:?}", axis_location);
            };
            let Some(user) = axis_location.get("Location") else {
                panic!("Incomprehensible axis location {:?}", axis_location);
            };
            let Some(user) = user.as_f64() else {
                panic!("Incomprehensible axis location {:?}", axis_location);
            };
            let Some(axis_index) = axis_index(from, |a| &a.name == axis_name) else {
                panic!("Axis has no index {:?}", axis_location);
            };
            let Some(axis_values) = master.axes_values.as_ref() else {
                panic!("Master has no axis values {:?}", master);
            };
            let user = user as f32;
            let design = axis_values[axis_index].into_inner() as f32;

            axis_mappings
                .entry(axis_name.clone())
                .or_default()
                .push((user.into(), design.into()));
        }
    }
    Some(axis_mappings)
}

fn extract_axis_mappings(from: &RawFont) -> BTreeMap<String, RawUserToDesignMapping> {
    let from_axis_mapping = user_to_design_from_axis_mapping(from);
    let from_axis_location = user_to_design_from_axis_location(from);
    match (from_axis_mapping, from_axis_location) {
        (Some(from_mapping), Some(..)) => {
            warn!("Axis Mapping *and* Axis Location are defined; using Axis Mapping");
            from_mapping
        }
        (Some(from_mapping), None) => from_mapping,
        (None, Some(from_location)) => from_location,
        (None, None) => BTreeMap::new(),
    }
}

impl TryFrom<RawShape> for Shape {
    type Error = Error;

    fn try_from(from: RawShape) -> Result<Self, Self::Error> {
        // TODO: handle numerous unsupported attributes
        // See <https://github.com/schriftgestalt/GlyphsSDK/blob/Glyphs3/GlyphsFileFormat/GlyphsFileFormatv3.md#differences-between-version-2>

        let shape = if let Some(Plist::String(..)) = from.other_stuff.get("ref") {
            // only components use ref in Glyphs 3
            Shape::Component(from.other_stuff.try_into()?)
        } else {
            // no ref; presume it's a path
            Shape::Path(Path {
                closed: from.closed.unwrap_or_default(),
                nodes: from.nodes.clone().unwrap_or_default(),
            })
        };
        Ok(shape)
    }
}

fn map_and_push_if_present<T, U>(dest: &mut Vec<T>, src: Option<Vec<U>>, map: fn(U) -> T) {
    let Some(src) = src else {
        return; // nop
    };
    src.into_iter().map(map).for_each(|v| dest.push(v));
}

impl TryFrom<RawLayer> for Layer {
    type Error = Error;

    fn try_from(from: RawLayer) -> Result<Self, Self::Error> {
        let mut shapes = Vec::new();

        // Glyphs v2 uses paths and components
        map_and_push_if_present(&mut shapes, from.paths, Shape::Path);
        map_and_push_if_present(&mut shapes, from.components, Shape::Component);

        // Glyphs v3 uses shapes for both
        for raw_shape in from.shapes.unwrap_or_default() {
            shapes.push(raw_shape.try_into()?);
        }

        Ok(Layer {
            layer_id: from.layer_id,
            width: from.width,
            shapes,
        })
    }
}

impl TryFrom<RawGlyph> for Glyph {
    type Error = Error;

    fn try_from(from: RawGlyph) -> Result<Self, Self::Error> {
        let mut instances = Vec::new();
        for layer in from.layers {
            // The presence of an associated master indicates this is not a simple instance
            // It's either a draft or a more complex usage, such as an alternate
            if layer.associated_master_id.is_some() {
                continue;
            }
            instances.push(layer.try_into()?);
        }
        Ok(Glyph {
            glyphname: from.glyphname,
            layers: instances,
        })
    }
}

impl TryFrom<RawFont> for Font {
    type Error = Error;

    fn try_from(mut from: RawFont) -> Result<Self, Self::Error> {
        if from.is_v2() {
            from.v2_to_v3()?;
        }

        let radix = if from.is_v2() { 16 } else { 10 };
        from.other_stuff.remove("date"); // exists purely to make diffs fail
        from.other_stuff.remove(".formatVersion"); // no longer relevent

        let glyph_order = parse_glyph_order(&from);
        let codepoints = parse_codepoints(&mut from, radix);

        let default_master_idx = default_master_idx(&from);
        let axis_mappings = extract_axis_mappings(&from);

        let mut glyphs = BTreeMap::new();
        for raw_glyph in from.glyphs.into_iter() {
            glyphs.insert(raw_glyph.glyphname.clone(), raw_glyph.try_into()?);
        }

        Ok(Font {
            family_name: from.family_name,
            axes: from.axes.unwrap_or_default(),
            font_master: from.font_master,
            default_master_idx,
            glyphs,
            glyph_order,
            codepoints,
            axis_mappings,
        })
    }
}

impl Font {
    pub fn load(glyphs_file: &path::Path) -> Result<Font, Error> {
        let raw_content = fs::read_to_string(glyphs_file).map_err(Error::IoError)?;

        // Glyphs has a wide variety of unicode definitions, not all of them parser friendly
        // Make unicode always a string, without any wrapping () so we can parse as csv, radix based on format version
        let re = Regex::new(
            r#"(?m)^(?P<prefix>\s*unicode\s*=\s*)[(]?(?P<value>[0-9a-zA-Z,]+)[)]?;\s*$"#,
        )
        .unwrap();
        let raw_content = re.replace_all(&raw_content, r#"$prefix"$value";"#);

        let mut raw_content = Plist::parse(&raw_content)
            .map_err(|e| Error::ParseError(glyphs_file.to_path_buf(), format!("{:#?}", e)))?;

        // Fix any issues with the raw plist
        let Plist::Dictionary(ref mut root_dict) = raw_content else {
            return Err(Error::ParseError(glyphs_file.to_path_buf(), "Root must be a dict".to_string()));
        };
        fix_glyphs_named_infinity(glyphs_file, root_dict)?;

        let raw_font = RawFont::from_plist(raw_content);
        raw_font.try_into()
    }

    pub fn default_master(&self) -> &FontMaster {
        &self.font_master[self.default_master_idx]
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
    };

    use crate::{Font, FromPlist, Node, Plist};

    use ordered_float::OrderedFloat;

    use pretty_assertions::assert_eq;

    fn testdata_dir() -> PathBuf {
        let dir = Path::new("../resources/testdata");
        assert!(dir.is_dir());
        dir.to_path_buf()
    }

    fn glyphs2_dir() -> PathBuf {
        testdata_dir().join("glyphs2")
    }

    fn glyphs3_dir() -> PathBuf {
        testdata_dir().join("glyphs3")
    }

    #[test]
    fn test_glyphs3_node() {
        assert_eq!(
            Node {
                node_type: crate::NodeType::Line,
                pt: super::Point {
                    x: 354.0.into(),
                    y: 183.0.into()
                }
            },
            Node::from_plist(Plist::Array(vec![
                Plist::Integer(354),
                Plist::Integer(183),
                Plist::String("l".into()),
            ]))
        );
    }

    #[test]
    fn test_glyphs2_node() {
        assert_eq!(
            Node {
                node_type: crate::NodeType::Line,
                pt: super::Point {
                    x: 354.0.into(),
                    y: 183.0.into()
                }
            },
            Node::from_plist(Plist::String("354 183 LINE".into()))
        );
    }

    // unquoted infinity likes to parse as a float which is suboptimal for glyph names. Survive.
    // Observed on Work Sans and Lexend.
    #[test]
    fn survive_unquoted_infinity() {
        // Read a minimal glyphs file that reproduces the error
        Font::load(&glyphs3_dir().join("infinity.glyphs")).unwrap();
    }

    #[test]
    fn read_2_and_3() {
        let g2 = Font::load(&glyphs2_dir().join("WghtVar.glyphs")).unwrap();
        let mut g3 = Font::load(&glyphs3_dir().join("WghtVar.glyphs")).unwrap();

        // for test purposes we are nto interested in icon name
        for master in g3.font_master.iter_mut() {
            master.other_stuff.remove("iconName");
        }

        assert_eq!(g2, g3);
    }

    #[test]
    fn upgrade_2_to_3_with_implicit_axes() {
        let font = Font::load(&glyphs2_dir().join("WghtVar_ImplicitAxes.glyphs")).unwrap();
        assert_eq!(
            font.axes
                .iter()
                .map(|a| a.tag.as_str())
                .collect::<Vec<&str>>(),
            vec!["wght", "wdth", "XXXX"]
        );
    }

    #[test]
    fn understand_v2_style_unquoted_hex_unicode() {
        let font = Font::load(&glyphs2_dir().join("Unicode-UnquotedHex.glyphs")).unwrap();
        assert_eq!("name", font.codepoints[&vec![0x1234]]);
        assert_eq!(1, font.glyphs.len());
    }

    #[test]
    fn understand_v2_style_quoted_hex_unicode_sequence() {
        let font = Font::load(&glyphs2_dir().join("Unicode-QuotedHexSequence.glyphs")).unwrap();
        assert_eq!("name", font.codepoints[&vec![0x2044, 0x200D, 0x2215]]);
        assert_eq!(1, font.glyphs.len());
    }

    #[test]
    fn understand_v3_style_unquoted_decimal_unicode() {
        let font = Font::load(&glyphs3_dir().join("Unicode-UnquotedDec.glyphs")).unwrap();
        assert_eq!("name", font.codepoints[&vec![182]]);
        assert_eq!(1, font.glyphs.len());
    }

    #[test]
    fn understand_v3_style_unquoted_decimal_unicode_sequence() {
        let font = Font::load(&glyphs3_dir().join("Unicode-UnquotedDecSequence.glyphs")).unwrap();
        assert_eq!("name", font.codepoints[&vec![1619, 1764]]);
        assert_eq!(1, font.glyphs.len());
    }

    #[test]
    fn axes_not_hidden() {
        let font = Font::load(&glyphs3_dir().join("WghtVar.glyphs")).unwrap();
        assert_eq!(
            font.axes.iter().map(|a| a.hidden).collect::<Vec<_>>(),
            vec![None]
        );
    }

    #[test]
    fn axis_hidden() {
        let font = Font::load(&glyphs3_dir().join("WghtVar_3master_CustomOrigin.glyphs")).unwrap();
        assert_eq!(
            font.axes.iter().map(|a| a.hidden).collect::<Vec<_>>(),
            vec![Some(true)]
        );
    }

    #[test]
    fn vf_origin_single_axis_default() {
        let font = Font::load(&glyphs3_dir().join("WghtVar.glyphs")).unwrap();
        assert_eq!(0, font.default_master_idx);
    }

    #[test]
    fn vf_origin_multi_axis_default() {
        let font = Font::load(&glyphs2_dir().join("WghtVar_ImplicitAxes.glyphs")).unwrap();
        assert_eq!(0, font.default_master_idx);
    }

    #[test]
    fn vf_origin_multi_axis_custom() {
        let font = Font::load(&glyphs3_dir().join("WghtVar_3master_CustomOrigin.glyphs")).unwrap();
        assert_eq!(2, font.default_master_idx);
    }

    #[test]
    fn glyph_order_default_is_file_order() {
        let font = Font::load(&glyphs3_dir().join("WghtVar.glyphs")).unwrap();
        assert_eq!(
            vec!["space", "exclam", "hyphen", "manual-component"],
            font.glyph_order
        );
    }

    #[test]
    fn glyph_order_override_obeyed() {
        let font = Font::load(&glyphs3_dir().join("WghtVar_GlyphOrder.glyphs")).unwrap();
        assert_eq!(vec!["hyphen", "space", "exclam"], font.glyph_order);
    }

    #[test]
    fn loads_global_axis_mappings_from_glyphs2() {
        let font = Font::load(&glyphs2_dir().join("OpszWghtVar_AxisMappings.glyphs")).unwrap();

        // Did you load the mappings? DID YOU?!
        assert_eq!(
            BTreeMap::from([
                (
                    "Optical Size".to_string(),
                    vec![
                        (OrderedFloat(12.0), OrderedFloat(12.0)),
                        (OrderedFloat(72.0), OrderedFloat(72.0))
                    ]
                ),
                (
                    "Weight".to_string(),
                    vec![
                        (OrderedFloat(100.0), OrderedFloat(40.0)),
                        (OrderedFloat(200.0), OrderedFloat(46.0)),
                        (OrderedFloat(300.0), OrderedFloat(51.0)),
                        (OrderedFloat(400.0), OrderedFloat(57.0)),
                        (OrderedFloat(500.0), OrderedFloat(62.0)),
                        (OrderedFloat(600.0), OrderedFloat(68.0)),
                        (OrderedFloat(700.0), OrderedFloat(73.0)),
                    ]
                ),
            ]),
            font.axis_mappings
        );
    }

    #[test]
    fn loads_global_axis_locations_from_glyphs3() {
        let font = Font::load(&glyphs3_dir().join("WghtVar_AxisLocation.glyphs")).unwrap();

        // Did you load the mappings? DID YOU?!
        assert_eq!(
            BTreeMap::from([(
                "Weight".to_string(),
                vec![
                    (OrderedFloat(400.0), OrderedFloat(0.0)),
                    (OrderedFloat(500.0), OrderedFloat(8.0)),
                    (OrderedFloat(700.0), OrderedFloat(10.0)),
                ]
            ),]),
            font.axis_mappings
        );
    }
}
