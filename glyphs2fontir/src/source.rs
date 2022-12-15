use fontir::coords::{CoordConverter, DesignCoord, UserCoord, DesignLocation};
use fontir::error::{Error, WorkError};
use fontir::ir::{self, StaticMetadata};
use fontir::orchestration::Context;
use fontir::source::{Input, Source, Work};
use fontir::stateset::StateSet;
use glyphs_reader::{Font, FontMaster};
use log::debug;
use ordered_float::OrderedFloat;
use std::collections::HashSet;
use std::sync::Arc;
use std::{collections::HashMap, path::PathBuf};

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
    font: Arc<Font>,
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
        let font = Font::load(&self.glyphs_file).map_err(|e| {
            Error::ParseError(
                self.glyphs_file.clone(),
                format!("Unable to read glyphs file: {}", e),
            )
        })?;
        let glyphs = glyph_states(&font)?;
        let global_metadata = self.global_rebuild_triggers(&font)?;

        self.cache = Some(Cache {
            global_metadata: global_metadata.clone(),
            font: Arc::from(font),
        });

        Ok(Input {
            global_metadata,
            glyphs,
        })
    }

    fn create_static_metadata_work(&self, context: &Context) -> Result<Box<dyn Work>, Error> {
        self.check_global_metadata(&context.input.global_metadata)?;
        Ok(Box::from(StaticMetadataWork {
            font: self.cache.as_ref().unwrap().font.clone(),
        }))
    }

    fn create_glyph_ir_work(
        &self,
        glyph_names: &HashSet<&str>,
        context: &Context,
    ) -> Result<Vec<Box<dyn Work>>, fontir::error::Error> {
        self.check_global_metadata(&context.input.global_metadata)?;

        let cache = self.cache.as_ref().unwrap();        

        let mut work: Vec<Box<dyn Work>> = Vec::new();
        for glyph_name in glyph_names {
            work.push(Box::from(self.create_work_for_one_glyph(glyph_name, cache.font.clone())?));
        }
        Ok(work)
    }
}

impl GlyphsIrSource {
    fn create_work_for_one_glyph(&self, glyph_name: &str, font: Arc<Font>) -> Result<GlyphIrWork, Error> {
        let glyph_name = glyph_name.to_string();
        let master_indices: HashMap<_, _> = font.font_master.iter()
            .enumerate()
            .map(|(idx, m)| (m.id.clone(), idx))
            .collect();
        Ok(GlyphIrWork { glyph_name, font, master_indices: Arc::from(master_indices)})
    }
}

struct StaticMetadataWork {
    font: Arc<Font>,
}

impl Work for StaticMetadataWork {
    fn exec(&self, context: &Context) -> Result<(), WorkError> {
        let font = self.font.as_ref();
        debug!("Static metadata for {}", font.family_name);

        let mut axis_values = Vec::new();
        for master in font.font_master.iter() {
            master
                .axes_values
                .as_ref()
                .ok_or_else(|| {
                    WorkError::InconsistentAxisDefinitions(format!(
                        "No axis values for {}",
                        master.id
                    ))
                })?
                .iter()
                .enumerate()
                .for_each(|(idx, value)| {
                    while axis_values.len() <= idx {
                        axis_values.push(Vec::new());
                    }
                    axis_values[idx].push(value);
                });
        }

        if font.axes.len() != axis_values.len() || axis_values.iter().any(|v| v.is_empty()) {
            return Err(WorkError::InconsistentAxisDefinitions(format!(
                "Axes {:?} doesn't match axis values {:?}",
                font.axes, axis_values
            )));
        }

        let axes = font
            .axes
            .iter()
            .enumerate()
            .map(|(idx, a)| {
                let min = axis_values[idx]
                    .iter()
                    .map(|v| OrderedFloat::<f32>(v.into_inner() as f32))
                    .min()
                    .unwrap();
                let max = axis_values[idx]
                    .iter()
                    .map(|v| OrderedFloat::<f32>(v.into_inner() as f32))
                    .max()
                    .unwrap();
                let default = OrderedFloat::<f32>(
                    axis_values[idx][font.default_master_idx].into_inner() as f32,
                );

                // Given in design coords based on a sample file
                let default = DesignCoord::new(default);
                let min = DesignCoord::new(min);
                let max = DesignCoord::new(max);

                // TODO: support Axis Location (https://glyphsapp.com/learn/creating-a-variable-font#g-axis-mappings-and-locations__option-1-axis-location-parameters)

                let converter = if font.axis_mappings.contains_key(&a.tag) {
                    let mappings: Vec<_> = font
                        .axis_mappings
                        .get(&a.tag)
                        .unwrap()
                        .iter()
                        .map(|(u, d)| (UserCoord::new(*u), DesignCoord::new(*d)))
                        .collect();
                    let default_idx = mappings
                        .iter()
                        .position(|(_, dc)| *dc == default)
                        .unwrap_or_else(|| {
                            panic!("Must have a mapping for default {:?} on {}", default, a.tag)
                        });
                    mappings
                        .iter()
                        .position(|(_, dc)| *dc == min)
                        .unwrap_or_else(|| {
                            panic!("Must have a mapping for min {:?} on {}", min, a.tag)
                        });
                    mappings
                        .iter()
                        .position(|(_, dc)| *dc == max)
                        .unwrap_or_else(|| {
                            panic!("Must have a mapping for max {:?} on {}", max, a.tag)
                        });
                    CoordConverter::new(mappings, default_idx)
                } else {
                    // There is no mapping that we can understand; let design == user
                    let min = UserCoord::new(min.into_inner());
                    let max = UserCoord::new(max.into_inner());
                    let default = UserCoord::new(default.into_inner());
                    CoordConverter::unmapped(min, default, max)
                };

                let default = default.to_user(&converter);
                let min = min.to_user(&converter);
                let max = max.to_user(&converter);

                ir::Axis {
                    name: a.name.clone(),
                    tag: a.tag.clone(),
                    hidden: a.hidden.unwrap_or(false),
                    min,
                    default,
                    max,
                    converter,
                }
            })
            .collect();

        context.set_static_metadata(StaticMetadata::new(axes, font.glyph_order.clone()));
        Ok(())
    }
}

fn location(axes: &Vec<glyphs_reader::Axis>, master: &FontMaster) -> DesignLocation {
    axes.iter().zip(master.axes_values.as_ref().unwrap())
        .map(|(axis, pos)| (axis.tag.clone(), DesignCoord::new(pos.into_inner() as f32)))
        .collect()    
}

struct GlyphIrWork {
    glyph_name: String,
    font: Arc<Font>,
    master_indices: Arc<HashMap<String, usize>>,
}

impl Work for GlyphIrWork {
    fn exec(&self, context: &Context) -> Result<(), WorkError> {
        debug!("Generate IR for {}", self.glyph_name);
        let font = self.font.as_ref();

        let static_metadata = context.get_static_metadata();

        let gid = static_metadata
            .glyph_id(&self.glyph_name)
            .ok_or_else(|| WorkError::NoGlyphIdForName(self.glyph_name.clone()))?;

        let glyph = font.glyphs.get(&self.glyph_name)
            .ok_or_else(|| WorkError::NoGlyphIdForName(self.glyph_name.clone()))?;    

        // Masters have locations

        
        // Glyphs have layers that match up with masters, and masters have locations
        for layer in glyph.layers.iter() {
            let master = &font.font_master[self.master_indices[&layer.layer_id]];
            let location = location(&font.axes, &master);
            
            todo!("{:#?}\n{}", master, location)
        }
        // And so we have shapes at locations

        let ir = ir::Glyph {
            name: self.glyph_name.clone(),
            sources: HashMap::new(),
        };
        context.set_glyph_ir(gid, ir);
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
        coords::{CoordConverter, DesignCoord, UserCoord},
        ir,
        orchestration::{Context, WorkIdentifier},
        source::{Paths, Source},
        stateset::StateSet,
    };
    use glyphs_reader::Font;

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
        let expected_keys = HashSet::from(["space", "hyphen", "exclam"]);
        assert_eq!(
            expected_keys,
            glyph_state_for_file(&glyphs3_dir(), "WghtVar.glyphs")
                .keys()
                .map(|k| k.as_str())
                .collect::<HashSet<&str>>()
        );
        assert_eq!(
            expected_keys,
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
            .create_static_metadata_work(&context)
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
        assert_eq!(
            vec!["space", "exclam", "hyphen"],
            context.get_static_metadata().glyph_order
        );
    }

    #[test]
    fn static_metadata_ir_multi_axis() {
        // Caused index out of bounds due to transposed master and value indices
        let (source, context) = context_for(glyphs2_dir().join("BadIndexing.glyphs"));
        let task_context = context.copy_for_work(WorkIdentifier::StaticMetadata, None);
        source
            .create_static_metadata_work(&context)
            .unwrap()
            .exec(&task_context)
            .unwrap();
    }

    #[test]
    fn loads_axis_mappings_from_glyphs2() {
        let (source, context) = context_for(glyphs2_dir().join("WghtVar_AxisMappings.glyphs"));
        let task_context = context.copy_for_work(WorkIdentifier::StaticMetadata, None);
        source
            .create_static_metadata_work(&context)
            .unwrap()
            .exec(&task_context)
            .unwrap();
        let static_metadata = context.get_static_metadata();

        // Did you load the mappings? DID YOU?!
        assert_eq!(
            vec![ir::Axis {
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
            }],
            static_metadata.axes
        );
    }

    #[test]
    fn glyph_ir() {
        let (source, context) = context_for(glyphs3_dir().join("WghtVar.glyphs"));
        source
            .create_static_metadata_work(&context)
            .unwrap()
            .exec(&context)
            .unwrap();
        let work = source
            .create_glyph_ir_work(&HashSet::from(["exclam"]), &context)
            .unwrap();
        assert_eq!(1, work.len());
        let work = &work[0];
        work.exec(&context).unwrap();

        let gid = context
            .get_static_metadata()
            .glyph_order
            .iter()
            .position(|g| g == "exclam")
            .unwrap();
        let glyph_ir = context.get_glyph_ir(gid as u32);
        assert_eq!("exclam", glyph_ir.name);
    }
}
