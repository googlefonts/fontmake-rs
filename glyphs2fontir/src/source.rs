use fontir::coords::NormalizedCoord;
use fontir::error::{Error, WorkError};
use fontir::ir::{self, GlyphInstance, StaticMetadata};
use fontir::orchestration::Context;
use fontir::source::{Input, Source, Work};
use fontir::stateset::StateSet;
use glyphs_reader::Font;
use indexmap::IndexSet;
use log::{debug, trace};
use std::collections::HashSet;
use std::sync::Arc;
use std::{collections::HashMap, path::PathBuf};

use crate::toir::FontInfo;

pub struct GlyphsIrSource {
    glyphs_file: PathBuf,
    cache: Option<Cache>,
}

impl GlyphsIrSource {
    pub fn new(glyphs_file: PathBuf) -> GlyphsIrSource {
        GlyphsIrSource {
            glyphs_file,
            cache: None,
        }
    }
}

struct Cache {
    global_metadata: StateSet,
    font_info: Arc<FontInfo>,
}

impl Cache {
    fn is_valid_for(&self, global_metadata: &StateSet) -> bool {
        self.global_metadata == *global_metadata
    }
}

fn glyph_identifier(glyph_name: &str) -> String {
    format!("/glyph/{glyph_name}")
}

fn glyph_states(font: &Font) -> Result<HashMap<String, StateSet>, Error> {
    let mut glyph_states = HashMap::new();

    for (glyphname, glyph) in font.glyphs.iter() {
        let mut state = StateSet::new();
        state.track_memory(glyph_identifier(glyphname), glyph)?;
        glyph_states.insert(glyphname.clone(), state);
    }

    Ok(glyph_states)
}

impl GlyphsIrSource {
    // When things like upem may have changed forget incremental and rebuild the whole thing
    fn global_rebuild_triggers(&self, font: &Font) -> Result<StateSet, Error> {
        let mut state = StateSet::new();
        // Wipe out glyph-related fields, track the rest
        // Explicitly field by field so if we add more compiler will force us to update here
        let font = Font {
            family_name: font.family_name.clone(),
            axes: font.axes.clone(),
            font_master: font.font_master.clone(),
            default_master_idx: font.default_master_idx,
            glyphs: Default::default(),
            glyph_order: Default::default(),
            codepoints: Default::default(),
            axis_mappings: font.axis_mappings.clone(),
        };
        state.track_memory("/font_master".to_string(), &font)?;
        Ok(state)
    }

    fn check_global_metadata(&self, global_metadata: &StateSet) -> Result<(), Error> {
        // Do we have a plist cache?
        // TODO: consider just recomputing here instead of failing
        if !self
            .cache
            .as_ref()
            .map(|pc| pc.is_valid_for(global_metadata))
            .unwrap_or(false)
        {
            return Err(Error::InvalidGlobalMetadata);
        }
        Ok(())
    }
}

impl Source for GlyphsIrSource {
    fn inputs(&mut self) -> Result<Input, Error> {
        // We have to read the glyphs file then shred it to figure out if anything changed
        let font_info = FontInfo::try_from(Font::load(&self.glyphs_file).map_err(|e| {
            Error::ParseError(
                self.glyphs_file.clone(),
                format!("Unable to read glyphs file: {}", e),
            )
        })?)?;
        let font = &font_info.font;
        let global_metadata = self.global_rebuild_triggers(font)?;

        let glyphs = glyph_states(font)?;

        self.cache = Some(Cache {
            global_metadata: global_metadata.clone(),
            font_info: Arc::from(font_info),
        });

        Ok(Input {
            global_metadata,
            glyphs,
        })
    }

    fn create_static_metadata_work(&self, input: &Input) -> Result<Box<dyn Work + Send>, Error> {
        self.check_global_metadata(&input.global_metadata)?;
        let font_info = self.cache.as_ref().unwrap().font_info.clone();
        Ok(Box::from(StaticMetadataWork { font_info }))
    }

    fn create_glyph_ir_work(
        &self,
        glyph_names: &IndexSet<&str>,
        input: &Input,
    ) -> Result<Vec<Box<dyn Work + Send>>, fontir::error::Error> {
        self.check_global_metadata(&input.global_metadata)?;

        let cache = self.cache.as_ref().unwrap();

        let mut work: Vec<Box<dyn Work + Send>> = Vec::new();
        for glyph_name in glyph_names {
            work.push(Box::from(
                self.create_work_for_one_glyph(glyph_name, cache.font_info.clone())?,
            ));
        }
        Ok(work)
    }
}

impl GlyphsIrSource {
    fn create_work_for_one_glyph(
        &self,
        glyph_name: &str,
        font_info: Arc<FontInfo>,
    ) -> Result<GlyphIrWork, Error> {
        let glyph_name = glyph_name.to_string();
        Ok(GlyphIrWork {
            glyph_name,
            font_info,
        })
    }
}

struct StaticMetadataWork {
    font_info: Arc<FontInfo>,
}

impl Work for StaticMetadataWork {
    fn exec(&self, context: &Context) -> Result<(), WorkError> {
        let font_info = self.font_info.as_ref();
        let font = &font_info.font;
        debug!("Static metadata for {}", font.family_name);
        context.set_static_metadata(StaticMetadata::new(
            font_info.axes.clone(),
            font.glyph_order.iter().cloned().collect(),
        ));
        Ok(())
    }
}

struct GlyphIrWork {
    glyph_name: String,
    font_info: Arc<FontInfo>,
}

fn check_pos(
    glyph_name: &str,
    positions: &HashSet<NormalizedCoord>,
    axis: &ir::Axis,
    pos: &NormalizedCoord,
) -> Result<(), WorkError> {
    if !positions.contains(pos) {
        return Err(WorkError::GlyphUndefAtNormalizedPosition {
            glyph_name: glyph_name.to_string(),
            axis: axis.tag.clone(),
            pos: *pos,
        });
    }
    Ok(())
}

impl Work for GlyphIrWork {
    fn exec(&self, context: &Context) -> Result<(), WorkError> {
        trace!("Generate IR for {}", self.glyph_name);
        let font_info = self.font_info.as_ref();
        let font = &font_info.font;

        let static_metadata = context.get_static_metadata();
        let axes = &static_metadata.axes;

        let glyph = font
            .glyphs
            .get(&self.glyph_name)
            .ok_or_else(|| WorkError::NoGlyphForName(self.glyph_name.clone()))?;

        let mut ir_glyph = ir::Glyph::new(self.glyph_name.clone());

        // Glyphs have layers that match up with masters, and masters have locations
        let mut axis_positions: HashMap<String, HashSet<NormalizedCoord>> = HashMap::new();
        for instance in glyph.layers.iter() {
            let Some(master_idx) = font_info.master_indices.get(instance.layer_id.as_str()) else {
                return Err(WorkError::NoMasterForGlyph {
                    master: instance.layer_id.clone(),
                    glyph: self.glyph_name.clone(),
                });
            };
            let master = &font.font_master[*master_idx];
            let location = &font_info.master_locations[master.id.as_str()];

            for (tag, coord) in location.iter() {
                axis_positions
                    .entry(tag.clone())
                    .or_default()
                    .insert(*coord);
            }

            // TODO actually populate fields
            let glyph_instance = GlyphInstance {
                width: instance.width.into_inner(),
                height: None,
                contours: Vec::new(),
                components: Vec::new(),
            };

            ir_glyph
                .try_add_source(location, glyph_instance)
                .map_err(|e| {
                    WorkError::AddGlyphSource(format!(
                        "Unable to add source to {} at {:?}: {}",
                        self.glyph_name.clone(),
                        location,
                        e
                    ))
                })?;
        }

        // It's helpful if glyphs are defined at min, default, and max (some of which may be cooincident)
        for axis in axes.iter() {
            let min = axis.min.to_normalized(&axis.converter);
            let max = axis.max.to_normalized(&axis.converter);
            let default = axis.max.to_normalized(&axis.converter);
            let positions = axis_positions.get(&axis.name).unwrap();
            check_pos(&self.glyph_name, positions, axis, &min)?;
            check_pos(&self.glyph_name, positions, axis, &default)?;
            check_pos(&self.glyph_name, positions, axis, &max)?;
        }

        context.set_glyph_ir(&self.glyph_name, ir_glyph);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        path::{Path, PathBuf},
    };

    use fontir::{
        coords::{
            CoordConverter, DesignCoord, NormalizedCoord, NormalizedLocation, UserCoord,
            UserLocation,
        },
        error::WorkError,
        ir,
        orchestration::{Context, WorkIdentifier},
        source::{Paths, Source},
        stateset::StateSet,
    };
    use glyphs_reader::Font;
    use indexmap::IndexSet;

    use super::{glyph_states, GlyphsIrSource};

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

    fn glyph_state_for_file(dir: &Path, filename: &str) -> HashMap<String, StateSet> {
        let glyphs_file = dir.join(filename);
        let font = Font::load(&glyphs_file).unwrap();
        glyph_states(&font).unwrap()
    }

    #[test]
    fn find_glyphs() {
        assert_eq!(
            HashSet::from(["space", "hyphen", "exclam", "manual-component"]),
            glyph_state_for_file(&glyphs3_dir(), "WghtVar.glyphs")
                .keys()
                .map(|k| k.as_str())
                .collect::<HashSet<&str>>()
        );
        assert_eq!(
            HashSet::from(["space", "hyphen", "exclam"]),
            glyph_state_for_file(&glyphs3_dir(), "WghtVar_HeavyHyphen.glyphs")
                .keys()
                .map(|k| k.as_str())
                .collect::<HashSet<&str>>()
        );
    }

    #[test]
    fn detect_changed_glyphs() {
        let keys = HashSet::from(["space", "hyphen", "exclam"]);

        let g1 = glyph_state_for_file(&glyphs3_dir(), "WghtVar.glyphs");
        let g2 = glyph_state_for_file(&glyphs3_dir(), "WghtVar_HeavyHyphen.glyphs");

        let changed = keys
            .iter()
            .filter_map(|key| {
                let key = key.to_string();
                if g1.get(&key).unwrap() == g2.get(&key).unwrap() {
                    return None;
                }
                Some(key)
            })
            .collect::<HashSet<String>>();
        assert_eq!(HashSet::from(["hyphen".to_string()]), changed);
    }

    fn context_for(glyphs_file: PathBuf) -> (impl Source, Context) {
        let mut source = GlyphsIrSource::new(glyphs_file);
        let input = source.inputs().unwrap();
        (
            source,
            Context::new_root(
                false,
                Paths::new(Path::new("/nothing/should/write/here")),
                input,
            ),
        )
    }

    #[test]
    fn static_metadata_ir() {
        let (source, context) = context_for(glyphs3_dir().join("WghtVar.glyphs"));
        let task_context = context.copy_for_work(WorkIdentifier::StaticMetadata, None);
        source
            .create_static_metadata_work(&context.input)
            .unwrap()
            .exec(&task_context)
            .unwrap();

        assert_eq!(
            vec!["wght"],
            context
                .get_static_metadata()
                .axes
                .iter()
                .map(|a| &a.tag)
                .collect::<Vec<_>>()
        );
        let expected: IndexSet<String> = vec!["space", "exclam", "hyphen", "manual-component"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(expected, context.get_static_metadata().glyph_order);
    }

    #[test]
    fn static_metadata_ir_multi_axis() {
        // Caused index out of bounds due to transposed master and value indices
        let (source, context) = context_for(glyphs2_dir().join("BadIndexing.glyphs"));
        let task_context = context.copy_for_work(WorkIdentifier::StaticMetadata, None);
        source
            .create_static_metadata_work(&context.input)
            .unwrap()
            .exec(&task_context)
            .unwrap();
    }

    #[test]
    fn loads_axis_mappings_from_glyphs2() {
        let (source, context) = context_for(glyphs2_dir().join("OpszWghtVar_AxisMappings.glyphs"));
        let task_context = context.copy_for_work(WorkIdentifier::StaticMetadata, None);
        source
            .create_static_metadata_work(&context.input)
            .unwrap()
            .exec(&task_context)
            .unwrap();
        let static_metadata = context.get_static_metadata();

        // Did you load the mappings? DID YOU?!
        assert_eq!(
            vec![
                ir::Axis {
                    name: "Weight".into(),
                    tag: "wght".into(),
                    min: UserCoord::new(100.0),
                    default: UserCoord::new(500.0),
                    max: UserCoord::new(700.0),
                    hidden: false,
                    converter: CoordConverter::new(
                        vec![
                            (UserCoord::new(100.0), DesignCoord::new(40.0)),
                            (UserCoord::new(200.0), DesignCoord::new(46.0)),
                            (UserCoord::new(300.0), DesignCoord::new(51.0)),
                            (UserCoord::new(400.0), DesignCoord::new(57.0)),
                            (UserCoord::new(500.0), DesignCoord::new(62.0)), // default
                            (UserCoord::new(600.0), DesignCoord::new(68.0)),
                            (UserCoord::new(700.0), DesignCoord::new(73.0)),
                        ],
                        4
                    ),
                },
                ir::Axis {
                    name: "Optical Size".into(),
                    tag: "opsz".into(),
                    min: UserCoord::new(12.0),
                    default: UserCoord::new(12.0),
                    max: UserCoord::new(72.0),
                    hidden: false,
                    converter: CoordConverter::new(
                        vec![
                            (UserCoord::new(12.0), DesignCoord::new(12.0)), // default
                            (UserCoord::new(72.0), DesignCoord::new(72.0)),
                        ],
                        0
                    ),
                },
            ],
            static_metadata.axes
        );
    }

    fn build_static_metadata(glyphs_file: PathBuf) -> (impl Source, Context) {
        let (source, context) = context_for(glyphs_file);
        let task_context = context.copy_for_work(WorkIdentifier::StaticMetadata, None);
        source
            .create_static_metadata_work(&context.input)
            .unwrap()
            .exec(&task_context)
            .unwrap();
        (source, context)
    }

    fn build_glyphs<'a>(
        source: &impl Source,
        context: &Context,
        glyph_names: Vec<&'a String>,
    ) -> Result<(), WorkError> {
        for glyph_name in glyph_names.iter() {
            let work_items = source
                .create_glyph_ir_work(&IndexSet::from([glyph_name.as_str()]), &context.input)
                .unwrap();
            for work in work_items.iter() {
                let task_context = context.copy_for_work(
                    WorkIdentifier::GlyphIr(glyph_name.to_string()),
                    Some(HashSet::from([WorkIdentifier::StaticMetadata])),
                );
                work.exec(&task_context)?;
            }
        }
        Ok(())
    }

    #[test]
    fn glyph_user_locations() {
        let glyph_name = "space".to_string();
        let (source, context) =
            build_static_metadata(glyphs2_dir().join("OpszWghtVar_AxisMappings.glyphs"));
        build_glyphs(&source, &context, vec![&glyph_name]).unwrap(); // we dont' care about geometry

        let static_metadata = context.get_static_metadata();
        let axes = static_metadata.axes.iter().map(|a| (&a.name, a)).collect();

        let mut expected_locations = HashSet::new();
        for (opsz, wght) in &[
            (12.0, 100.0),
            (12.0, 500.0),
            (12.0, 700.0),
            (72.0, 100.0),
            (72.0, 500.0),
            (72.0, 700.0),
        ] {
            let mut loc = UserLocation::new();
            loc.set_pos("Optical Size", UserCoord::new(*opsz));
            loc.set_pos("Weight", UserCoord::new(*wght));
            let loc: _ = loc;
            expected_locations.insert(loc);
        }
        let actual_locations = context
            .get_glyph_ir(&glyph_name)
            .sources
            .keys()
            .map(|c| c.to_user(&axes))
            .collect::<HashSet<_>>();

        assert_eq!(expected_locations, actual_locations);
    }

    #[test]
    fn glyph_normalized_locations() {
        let glyph_name = "space".to_string();
        let (source, context) =
            build_static_metadata(glyphs2_dir().join("OpszWghtVar_AxisMappings.glyphs"));
        build_glyphs(&source, &context, vec![&glyph_name]).unwrap(); // we dont' care about geometry

        let mut expected_locations = HashSet::new();
        for (opsz, wght) in &[
            (0.0, -1.0),
            (0.0, 0.0),
            (0.0, 1.0),
            (1.0, -1.0),
            (1.0, 0.0),
            (1.0, 1.0),
        ] {
            let mut loc = NormalizedLocation::new();
            loc.set_pos("Optical Size", NormalizedCoord::new(*opsz));
            loc.set_pos("Weight", NormalizedCoord::new(*wght));
            let loc: _ = loc;
            expected_locations.insert(loc);
        }
        let actual_locations = context
            .get_glyph_ir(&glyph_name)
            .sources
            .keys()
            .cloned()
            .collect::<HashSet<_>>();

        assert_eq!(expected_locations, actual_locations);
    }

    #[test]
    fn glyph_must_define_min() {
        let glyph_name = "min-undefined".to_string();
        let (source, context) = build_static_metadata(glyphs2_dir().join("MinUndef.glyphs"));
        let result = build_glyphs(&source, &context, vec![&glyph_name]);
        assert!(result.is_err());
        let Err(WorkError::GlyphUndefAtNormalizedPosition { glyph_name, axis, pos }) =  result else {
            panic!("Wrong error");
        };
        assert_eq!("min-undefined", glyph_name);
        assert_eq!("wght", axis);
        assert_eq!(NormalizedCoord::new(-1.0), pos);
    }

    #[test]
    fn read_axis_location() {
        let (_, context) = build_static_metadata(glyphs3_dir().join("WghtVar_AxisLocation.glyphs"));
        let wght = &context.get_static_metadata().axes;
        assert_eq!(1, wght.len());
        let wght = &wght[0];

        for (design, user) in &[
            (0.0, 400.0),
            (4.0, 450.0),
            (8.0, 500.0),
            (9.0, 600.0),
            (10.0, 700.0),
        ] {
            assert_eq!(
                DesignCoord::new(*design),
                UserCoord::new(*user).to_design(&wght.converter),
                "{:#?}",
                wght.converter
            );
        }
    }
}
