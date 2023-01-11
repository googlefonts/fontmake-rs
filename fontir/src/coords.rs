//! Using confusable types for coords is an endless source of confusion; don't.
//!
//! See <https://github.com/googlefonts/fontmake-rs/blob/main/resources/text/units.md>

use std::collections::{BTreeMap, HashMap};

use crate::serde::CoordConverterSerdeRepr;
use crate::{ir::Axis, piecewise_linear_map::PiecewiseLinearMap};
use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};

/// A coordinate in some arbitrary space the designer dreamed up.
///
/// In .designspace, an xvalue. <https://fonttools.readthedocs.io/en/latest/designspaceLib/xml.html#dimension-element>.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DesignCoord(OrderedFloat<f32>);

/// A coordinate the end user sees, e.g. what 'fvar' uses, Weight 400.
///
/// In .designspace, a uservalue. <https://fonttools.readthedocs.io/en/latest/designspaceLib/xml.html#dimension-element>.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserCoord(OrderedFloat<f32>);

/// A coordinate used within the font, not seen by any user.
///
/// Always in [-1, 1].
///
/// Not typically used directly in sources.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedCoord(OrderedFloat<f32>);

/// A set of per-axis coordinates that define a specific location in a coordinate system.
///
/// E.g. a user location is a `Location<UserCoord>`. Hashable so it can do things like be
/// the key for a map of sources by location.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location<T>(BTreeMap<String, T>);

pub type DesignLocation = Location<DesignCoord>;
pub type UserLocation = Location<UserCoord>;
pub type NormalizedLocation = Location<NormalizedCoord>;

/// Converts between Design, User, and Normalized coordinates.
///
/// Stores [PiecewiseLinearMap]'s in several directions. Sources
/// suggest <= 10 mappings is typical, we can afford the bytes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(from = "CoordConverterSerdeRepr", into = "CoordConverterSerdeRepr")]
pub struct CoordConverter {
    pub(crate) default_idx: usize,
    pub(crate) user_to_design: PiecewiseLinearMap,
    design_to_user: PiecewiseLinearMap,
    design_to_normalized: PiecewiseLinearMap,
    normalized_to_design: PiecewiseLinearMap,
}

impl CoordConverter {
    /// Initialize a converter from the User:Design examples source files typically provide.
    pub fn new(mut mappings: Vec<(UserCoord, DesignCoord)>, default_idx: usize) -> CoordConverter {
        if mappings.is_empty() {
            mappings.push((UserCoord(0.0.into()), DesignCoord(0.0.into())));
        }
        let user_to_design = PiecewiseLinearMap::new(
            mappings
                .iter()
                .map(|(u, d)| (u.into_inner(), d.into_inner()))
                .collect(),
        );

        let design_coords: Vec<_> = mappings.iter().map(|(_, d)| d).collect();
        let design_min = design_coords.iter().min().unwrap();
        let design_max = design_coords.iter().max().unwrap();
        let design_default = design_coords[default_idx];

        let mut examples = Vec::new();
        if *design_min < design_default {
            examples.push((design_min.into_inner(), (-1.0).into())); // leftmost of default *must* be -1
        }
        examples.push((design_default.into_inner(), 0.0.into())); // default *must* land at 0
        if *design_max > design_default {
            examples.push((design_max.into_inner(), 1.0.into())); // right of default *must* be +1
        }
        let design_to_normalized = PiecewiseLinearMap::new(examples);

        let design_to_user = user_to_design.reverse();
        let normalized_to_design = design_to_normalized.reverse();

        CoordConverter {
            default_idx,
            user_to_design,
            design_to_user,
            design_to_normalized,
            normalized_to_design,
        }
    }

    /// Initialize a converter from just min/default/max user coords, e.g. a source with no mapping
    pub fn unmapped(min: UserCoord, default: UserCoord, max: UserCoord) -> CoordConverter {
        CoordConverter::new(
            vec![
                (min, DesignCoord::new(min.into_inner())),
                (default, DesignCoord::new(default.into_inner())),
                (max, DesignCoord::new(max.into_inner())),
            ],
            1,
        )
    }
}

impl DesignCoord {
    /// We do *not* provide From because we want conversion to be explicit
    pub fn new(value: impl Into<OrderedFloat<f32>>) -> DesignCoord {
        DesignCoord(value.into())
    }

    pub fn to_user(&self, converter: &CoordConverter) -> UserCoord {
        UserCoord::new(converter.design_to_user.map(self.0))
    }

    pub fn to_normalized(&self, converter: &CoordConverter) -> NormalizedCoord {
        NormalizedCoord::new(converter.design_to_normalized.map(self.0))
    }
}

impl UserCoord {
    /// We do *not* provide From because we want conversion to be explicit
    pub fn new(value: impl Into<OrderedFloat<f32>>) -> UserCoord {
        UserCoord(value.into())
    }

    pub fn to_design(&self, converter: &CoordConverter) -> DesignCoord {
        DesignCoord::new(converter.user_to_design.map(self.0))
    }

    pub fn to_normalized(&self, converter: &CoordConverter) -> NormalizedCoord {
        self.to_design(converter).to_normalized(converter)
    }
}

impl NormalizedCoord {
    /// We do *not* provide From because we want conversion to be explicit
    pub fn new(value: impl Into<OrderedFloat<f32>>) -> NormalizedCoord {
        NormalizedCoord(value.into())
    }

    pub fn to_design(&self, converter: &CoordConverter) -> DesignCoord {
        DesignCoord::new(converter.normalized_to_design.map(self.0))
    }

    pub fn to_user(&self, converter: &CoordConverter) -> UserCoord {
        self.to_design(converter).to_user(converter)
    }
}

impl DesignCoord {
    pub fn into_inner(self) -> OrderedFloat<f32> {
        self.0
    }
}

impl UserCoord {
    pub fn into_inner(self) -> OrderedFloat<f32> {
        self.0
    }
}

impl NormalizedCoord {
    pub fn into_inner(self) -> OrderedFloat<f32> {
        self.0
    }
}

impl<T> FromIterator<(String, T)> for Location<T> {
    fn from_iter<I: IntoIterator<Item = (String, T)>>(iter: I) -> Self {
        Location(BTreeMap::from_iter(iter))
    }
}

impl<T> Location<T> {
    pub fn new() -> Location<T> {
        Location(BTreeMap::new())
    }

    pub fn set_pos(&mut self, axis: impl Into<String>, pos: T) -> &mut Location<T> {
        self.0.insert(axis.into(), pos);
        self
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &T)> {
        self.0.iter()
    }
}

impl<T> Default for Location<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl DesignLocation {
    pub fn to_normalized(&self, axes: &HashMap<&String, &Axis>) -> NormalizedLocation {
        Location::<NormalizedCoord>(
            self.0
                .iter()
                .map(|(name, dc)| {
                    (
                        name.clone(),
                        dc.to_normalized(&axes.get(name).unwrap().converter),
                    )
                })
                .collect(),
        )
    }
}

impl NormalizedLocation {
    pub fn to_user(&self, axes: &HashMap<&String, &Axis>) -> UserLocation {
        Location::<UserCoord>(
            self.0
                .iter()
                .map(|(name, coord)| {
                    (
                        name.clone(),
                        coord.to_user(&axes.get(name).unwrap().converter),
                    )
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use ordered_float::OrderedFloat;

    use super::{CoordConverter, DesignCoord, UserCoord};

    // From <https://github.com/googlefonts/fontmake-rs/blob/main/resources/text/units.md>
    fn lexend_weight_mapping() -> (Vec<(UserCoord, DesignCoord)>, usize) {
        (
            vec![
                (UserCoord(100.0.into()), DesignCoord(26.0.into())),
                (UserCoord(200.0.into()), DesignCoord(39.0.into())),
                (UserCoord(300.0.into()), DesignCoord(58.0.into())),
                (UserCoord(400.0.into()), DesignCoord(90.0.into())), // [3]; default
                (UserCoord(500.0.into()), DesignCoord(108.0.into())),
                (UserCoord(600.0.into()), DesignCoord(128.0.into())),
                (UserCoord(700.0.into()), DesignCoord(151.0.into())),
                (UserCoord(800.0.into()), DesignCoord(169.0.into())),
                (UserCoord(900.0.into()), DesignCoord(190.0.into())),
            ],
            3,
        )
    }

    // 200 and 500 (user) are pushed way toward the left/right respectively
    fn bendy_mapping() -> (Vec<(UserCoord, DesignCoord)>, usize) {
        (
            vec![
                (UserCoord(100.0.into()), DesignCoord(0.0.into())),
                (UserCoord(200.0.into()), DesignCoord(1.0.into())),
                (UserCoord(400.0.into()), DesignCoord(10.0.into())), // [2]; default
                (UserCoord(500.0.into()), DesignCoord(19.0.into())),
                (UserCoord(900.0.into()), DesignCoord(20.0.into())),
            ],
            2,
        )
    }

    #[test]
    pub fn lexend_weight_internal_basics() {
        let (examples, default_idx) = lexend_weight_mapping();
        let converter = CoordConverter::new(examples, default_idx);
        assert_eq!(
            OrderedFloat(-1.0),
            DesignCoord(26.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(0.0),
            DesignCoord(90.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(1.0),
            DesignCoord(190.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
    }

    #[test]
    pub fn design_to_normalized_does_not_bend() {
        let (examples, default_idx) = bendy_mapping();
        let converter = CoordConverter::new(examples, default_idx);

        // 200 and 500 (user) are pushed way toward the left/right respectively
        // But design:normalized doesn't care, it's linear from default=>max and default=>min
        assert_eq!(
            OrderedFloat(-1.0),
            DesignCoord(0.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(-0.5),
            DesignCoord(5.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(0.0),
            DesignCoord(10.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(0.5),
            DesignCoord(15.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(1.0),
            DesignCoord(20.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
    }

    #[test]
    pub fn user_to_design_or_normalized_does_bend() {
        let (examples, default_idx) = bendy_mapping();
        let converter = CoordConverter::new(examples, default_idx);

        // 200 and 500 (user) are pushed way toward the left/right respectively

        // User : Design warps; 100..200 are squeezed into a small leftward slice
        // 150 is halfway between 100 and 200
        assert_eq!(
            OrderedFloat(0.0),
            UserCoord(100.0.into()).to_design(&converter).into_inner()
        );
        assert_eq!(
            OrderedFloat(0.5),
            UserCoord(150.0.into()).to_design(&converter).into_inner()
        );
        assert_eq!(
            OrderedFloat(1.0),
            UserCoord(200.0.into()).to_design(&converter).into_inner()
        );

        assert_eq!(
            OrderedFloat(-1.0),
            UserCoord(100.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(-0.95),
            UserCoord(150.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(-0.9),
            UserCoord(200.0.into())
                .to_normalized(&converter)
                .into_inner()
        );

        // 200..400 covers a massive slice!
        // 300 is halway to 400 (breaking news!)
        assert_eq!(
            OrderedFloat(5.5),
            UserCoord(300.0.into()).to_design(&converter).into_inner()
        );
        assert_eq!(
            OrderedFloat(10.0),
            UserCoord(400.0.into()).to_design(&converter).into_inner()
        );

        assert_eq!(
            OrderedFloat(-0.45),
            UserCoord(300.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
        assert_eq!(
            OrderedFloat(0.0),
            UserCoord(400.0.into())
                .to_normalized(&converter)
                .into_inner()
        );
    }
}
