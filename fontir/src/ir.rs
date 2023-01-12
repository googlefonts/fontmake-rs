//! Serde types for font IR.

use crate::{
    coords::{CoordConverter, NormalizedLocation, UserCoord},
    error::Error,
    serde::StaticMetadataSerdeRepr,
};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Global font info that cannot vary.
///
/// For example, upem, axis definitions, etc, as distinct from
/// metadata that varies across design space such as ascender/descender.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(from = "StaticMetadataSerdeRepr", into = "StaticMetadataSerdeRepr")]
pub struct StaticMetadata {
    pub axes: Vec<Axis>,
    pub glyph_order: IndexSet<String>,
}

impl StaticMetadata {
    pub fn new(axes: Vec<Axis>, glyph_order: IndexSet<String>) -> StaticMetadata {
        StaticMetadata { axes, glyph_order }
    }
}

impl StaticMetadata {
    pub fn glyph_id(&self, name: &String) -> Option<u32> {
        self.glyph_order.get_index_of(name).map(|i| i as u32)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Axis {
    pub name: String,
    pub tag: String,
    pub min: UserCoord,
    pub default: UserCoord,
    pub max: UserCoord,
    pub hidden: bool,
    pub converter: CoordConverter,
}

/// Features (Adobe fea).
///
/// In time will split gpos/gsub, have different features for different
/// locations, etc.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Features {
    pub fea_file: Option<PathBuf>,
}

impl Features {
    pub fn empty() -> Features {
        Features { fea_file: None }
    }
    pub fn from_file(file: &Path) -> Features {
        Features {
            fea_file: Some(file.to_path_buf()),
        }
    }
}

/// A variable definition of a single glyph.
///
/// Defined in at least one position. If defined in
/// many, presumed to vary continuously between positions and required
/// to have variation compatible structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Glyph {
    pub name: String,
    pub sources: HashMap<NormalizedLocation, GlyphInstance>,
}

impl Glyph {
    pub fn new(name: String) -> Self {
        Self {
            name,
            sources: HashMap::new(),
        }
    }

    pub fn try_add_source(
        &mut self,
        unique_location: &NormalizedLocation,
        source: GlyphInstance,
    ) -> Result<(), Error> {
        if self.sources.contains_key(unique_location) {
            return Err(Error::DuplicateNormalizedLocation {
                what: format!("glyph '{}' source", self.name),
                loc: unique_location.clone(),
            });
        }
        self.sources.insert(unique_location.clone(), source);
        Ok(())
    }
}

/// A Glyph at a specific position in designspace.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct GlyphInstance {
    /// Advance width.
    pub width: f64,
    /// Advance height; if None, assumed to equal font's ascender - descende.
    pub height: Option<f64>,
    /// List of glyph contours.
    pub contours: Vec<Contour>,
    /// List of glyph components.
    pub components: Vec<Component>,
}

/// A single glyph contour consisting of a list of points.
pub type Contour = Vec<ContourPoint>;

/// A single point in a glyph contour.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ContourPoint {
    pub x: f64,
    pub y: f64,
    pub typ: PointType,
}

/// Possible types of a point in a glyph contour, following UFO GLIF semantics.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum PointType {
    Move,
    Line,
    OffCurve,
    Curve,
    QCurve,
}

/// A single glyph component, reference to another glyph.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Component {
    /// The name of the referenced glyph.
    pub base: String,
    /// Affine transformation to apply to the referenced glyph.
    pub transform: Affine2x3,
}

/// A 2×3 affine transformation matrix.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Affine2x3 {
    /// x-component of transformed x-basis vector.
    pub xx: f64,
    /// y-component of transformed x-basis vector.
    pub yx: f64,
    /// x-component of transformed y-basis vector.
    pub xy: f64,
    /// y-component of transformed y-basis vector.
    pub yy: f64,
    /// x-component of translation vector.
    pub dx: f64,
    /// y-component of translation vector.
    pub dy: f64,
}

#[cfg(test)]
mod tests {
    use crate::{
        coords::{CoordConverter, NormalizedCoord, NormalizedLocation, UserCoord},
        ir::{Axis, Glyph, GlyphInstance},
    };

    fn test_axis() -> Axis {
        let min = UserCoord::new(100.0);
        let default = UserCoord::new(400.0);
        let max = UserCoord::new(900.0);
        let converter = CoordConverter::unmapped(min, default, max);
        Axis {
            name: String::from("Weight"),
            tag: String::from("wght"),
            min,
            default,
            max,
            hidden: false,
            converter,
        }
    }

    #[test]
    fn axis_yaml() {
        let test_axis = test_axis();
        let yml = serde_yaml::to_string(&test_axis).unwrap();
        assert_eq!(test_axis, serde_yaml::from_str(&yml).unwrap());
    }

    #[test]
    fn axis_bincode() {
        let test_axis = test_axis();
        let bin = bincode::serialize(&test_axis).unwrap();
        assert_eq!(test_axis, bincode::deserialize(&bin).unwrap());
    }

    #[test]
    fn glyph_yml() {
        let mut glyph = Glyph::new(String::from("A"));

        let loc1 = NormalizedLocation::on_axis("Weight", NormalizedCoord::new(0.0));
        glyph
            .try_add_source(&loc1, GlyphInstance::default())
            .unwrap();

        // // Uncomment lines below to see libyaml error disappear...
        // let loc2 = NormalizedLocation::on_axis("Weight", NormalizedCoord::new(1.0));
        // glyph.try_add_source(&loc2, GlyphInstance::default()).unwrap();

        println!("{:#?}", glyph);

        let yml = serde_yaml::to_string(&glyph).unwrap();
        println!("{}", yml);

        assert_eq!(yml, "TODO");
    }
}
