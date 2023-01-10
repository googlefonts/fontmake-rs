use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use fontir::{
    coords::{DesignLocation, NormalizedLocation, UserCoord},
    error::{Error, WorkError},
    ir::{Axis, StaticMetadata},
    orchestration::Context,
    source::{Input, Source, Work},
    stateset::{StateIdentifier, StateSet},
};
use indexmap::IndexSet;
use log::{debug, warn};
use norad::designspace::{self, DesignSpaceDocument};

use crate::toir::{to_design_location, to_ir_axis, to_ir_glyph};

pub struct DesignSpaceIrSource {
    designspace_file: PathBuf,
    designspace_dir: PathBuf,
    cache: Option<Cache>,
}

impl DesignSpaceIrSource {
    pub fn new(designspace_file: PathBuf) -> DesignSpaceIrSource {
        let designspace_dir = designspace_file
            .parent()
            .expect("designspace file *must* be in a directory")
            .to_path_buf();
        DesignSpaceIrSource {
            designspace_file,
            designspace_dir,
            cache: None,
        }
    }
}

// A cache of locations, valid provided no global metadata changes
struct Cache {
    global_metadata_sources: StateSet,
    locations: HashMap<PathBuf, Vec<DesignLocation>>,
    glyph_names: Arc<HashSet<String>>,
    designspace_file: PathBuf,
    designspace: Arc<DesignSpaceDocument>,
}

impl Cache {
    fn new(
        global_metadata_sources: StateSet,
        glyph_names: HashSet<String>,
        locations: HashMap<PathBuf, Vec<DesignLocation>>,
        designspace_file: PathBuf,
        designspace: DesignSpaceDocument,
    ) -> Cache {
        Cache {
            global_metadata_sources,
            glyph_names: Arc::from(glyph_names),
            locations,
            designspace_file,
            designspace: Arc::from(designspace),
        }
    }

    fn is_valid_for(&self, global_metadata_sources: &StateSet) -> bool {
        self.global_metadata_sources == *global_metadata_sources
    }

    fn location_of(&self, glif_file: &Path) -> Option<&Vec<DesignLocation>> {
        self.locations.get(glif_file)
    }
}

fn glif_files<'a>(
    ufo_dir: &Path,
    layer_cache: &'a mut HashMap<String, HashMap<String, PathBuf>>,
    source: &designspace::Source,
) -> Result<BTreeMap<String, PathBuf>, Error> {
    let layer_name = layer_dir(ufo_dir, layer_cache, source)?;
    let glyph_dir = ufo_dir.join(layer_name);
    if !glyph_dir.is_dir() {
        return Err(Error::DirectoryExpected(glyph_dir));
    }

    let glyph_list_file = glyph_dir.join("contents.plist");
    if !glyph_list_file.is_file() {
        return Err(Error::FileExpected(glyph_list_file));
    }
    let result: BTreeMap<String, PathBuf> = plist::from_file(&glyph_list_file)
        .map_err(|e| Error::ParseError(glyph_list_file, e.to_string()))?;

    Ok(result
        .into_iter()
        .map(|(glyph_name, path)| (glyph_name, glyph_dir.join(path)))
        .collect())
}

fn layer_contents(ufo_dir: &Path) -> Result<HashMap<String, PathBuf>, Error> {
    let file = ufo_dir.join("layercontents.plist");
    if !file.is_file() {
        return Err(Error::FileExpected(file));
    }
    let contents: Vec<(String, PathBuf)> =
        plist::from_file(&file).map_err(|e| Error::ParseError(file, e.to_string()))?;
    Ok(contents.into_iter().collect())
}

pub(crate) fn layer_dir<'a>(
    ufo_dir: &Path,
    layer_cache: &'a mut HashMap<String, HashMap<String, PathBuf>>,
    source: &designspace::Source,
) -> Result<&'a PathBuf, Error> {
    if !layer_cache.contains_key(&source.filename) {
        let contents = layer_contents(ufo_dir)?;
        layer_cache.insert(source.filename.clone(), contents);
    }
    let name_to_path = layer_cache.get_mut(&source.filename).unwrap();

    // No answer means dir is glyphs, which we'll stuff in under an empty string so the lifetime checks out
    let default_name = String::new();
    if source.layer.is_none() {
        name_to_path.insert(default_name.clone(), PathBuf::from("glyphs"));
    }
    name_to_path
        .get(source.layer.as_ref().unwrap_or(&default_name))
        .ok_or_else(|| Error::NoSuchLayer(source.filename.clone()))
}

impl DesignSpaceIrSource {
    fn load_designspace(&self) -> Result<DesignSpaceDocument, Error> {
        DesignSpaceDocument::load(&self.designspace_file)
            .map_err(|e| Error::UnableToLoadSource(Box::from(e)))
    }

    // When things like upem may have changed forget incremental and rebuild the whole thing
    fn global_rebuild_triggers(
        &self,
        designspace: &DesignSpaceDocument,
    ) -> Result<StateSet, Error> {
        let mut font_info = StateSet::new();
        font_info.track_file(&self.designspace_file)?;
        let (default_master_idx, _) = default_master(designspace)
            .ok_or_else(|| Error::NoDefaultMaster(self.designspace_file.clone()))?;

        for (idx, source) in designspace.sources.iter().enumerate() {
            let ufo_dir = self.designspace_dir.join(&source.filename);
            for filename in ["fontinfo.plist", "layercontents.plist", "lib.plist"] {
                // Only track lib.plist for the default master
                if filename == "lib.plist" && idx != default_master_idx {
                    continue;
                }

                let font_info_file = ufo_dir.join(filename);
                // TODO: this is incorrect; several of these files are optional
                // File tracking curently assumes you only track extant files so keep it for now
                if !font_info_file.is_file() {
                    return Err(Error::FileExpected(font_info_file));
                }
                font_info.track_file(&font_info_file)?;
            }
        }
        Ok(font_info)
    }

    fn check_global_metadata(&self, global_metadata: &StateSet) -> Result<(), Error> {
        // Do we have the location of glifs written down?
        // TODO: consider just recomputing here instead of failing
        if !self
            .cache
            .as_ref()
            .map(|gl| gl.is_valid_for(global_metadata))
            .unwrap_or(false)
        {
            return Err(Error::InvalidGlobalMetadata);
        }
        Ok(())
    }
}

impl Source for DesignSpaceIrSource {
    fn inputs(&mut self) -> Result<Input, Error> {
        let designspace = self.load_designspace()?;
        let global_metadata = self.global_rebuild_triggers(&designspace)?;

        // glif filenames are not reversible so we need to read contents.plist to figure out groups
        // See https://github.com/unified-font-object/ufo-spec/issues/164.
        let mut glyphs: HashMap<String, StateSet> = HashMap::new();

        // UFO filename => map of layer
        let mut layer_cache = HashMap::new();
        let mut glif_locations: HashMap<PathBuf, Vec<DesignLocation>> = HashMap::new();
        let mut glyph_names = HashSet::new();

        let Some((default_master_idx, default_master)) = default_master(&designspace) else {
            return Err(Error::NoDefaultMaster(self.designspace_file.clone()));
        };
        let mut sources_default_first = vec![default_master];
        sources_default_first.extend(
            designspace
                .sources
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != default_master_idx)
                .map(|(_, s)| s),
        );

        for (idx, source) in sources_default_first.iter().enumerate() {
            // Track files within each UFO
            // The UFO dir *must* exist since we were able to find fontinfo in it earlier
            let ufo_dir = self.designspace_dir.join(&source.filename);

            let location = to_design_location(&source.location);

            for (glyph_name, glif_file) in glif_files(&ufo_dir, &mut layer_cache, source)? {
                if !glif_file.exists() {
                    return Err(Error::FileExpected(glif_file));
                }
                if idx > 0 && !glyph_names.contains(&glyph_name) {
                    warn!("The glyph name '{}' exists in {} but not in the default master and will be ignored", glyph_name, source.filename);
                    continue;
                }
                glyph_names.insert(glyph_name.clone());
                let glif_file = glif_file.clone();
                glyphs
                    .entry(glyph_name)
                    .or_default()
                    .track_file(&glif_file)?;
                let glif_locations = glif_locations.entry(glif_file).or_default();
                glif_locations.push(location.clone());
            }
        }

        self.cache = Some(Cache::new(
            global_metadata.clone(),
            glyph_names,
            glif_locations,
            self.designspace_file.clone(),
            designspace,
        ));

        Ok(Input {
            global_metadata,
            glyphs,
        })
    }

    fn create_static_metadata_work(&self, input: &Input) -> Result<Box<dyn Work + Send>, Error> {
        self.check_global_metadata(&input.global_metadata)?;
        let cache = self.cache.as_ref().unwrap();

        Ok(Box::from(StaticMetadataWork {
            designspace_file: cache.designspace_file.clone(),
            designspace: cache.designspace.clone(),
            glyph_names: cache.glyph_names.clone(),
        }))
    }

    fn create_glyph_ir_work(
        &self,
        glyph_names: &IndexSet<&str>,
        input: &Input,
    ) -> Result<Vec<Box<dyn Work + Send>>, Error> {
        self.check_global_metadata(&input.global_metadata)?;

        // A single glif could be used by many source blocks that use the same layer
        // *gasp*
        // So resolve each file to 1..N locations in designspace
        let mut work: Vec<Box<dyn Work + Send>> = Vec::new();

        for glyph_name in glyph_names {
            work.push(Box::from(
                self.create_work_for_one_glyph(glyph_name, input)?,
            ));
        }

        Ok(work)
    }
}

impl DesignSpaceIrSource {
    fn create_work_for_one_glyph(
        &self,
        glyph_name: &str,
        input: &Input,
    ) -> Result<GlyphIrWork, Error> {
        // A single glif could be used by many source blocks that use the same layer
        // *gasp*
        // So resolve each file to 1..N locations in designspace

        let glyph_name = glyph_name.to_string();
        let stateset = input
            .glyphs
            .get(&glyph_name)
            .ok_or_else(|| Error::NoStateForGlyph(glyph_name.clone()))?;
        let mut glif_files = HashMap::new();
        let cache = self.cache.as_ref().unwrap();
        for state_key in stateset.keys() {
            let StateIdentifier::File(glif_file) = state_key else {
                return Err(Error::UnexpectedState);
            };
            let locations = cache
                .location_of(glif_file)
                .ok_or_else(|| Error::NoLocationsForGlyph(glyph_name.clone()))?;
            glif_files.insert(glif_file.to_path_buf(), locations.clone());
        }
        Ok(GlyphIrWork {
            glyph_name: glyph_name.clone(),
            glif_files,
        })
    }
}

struct StaticMetadataWork {
    designspace_file: PathBuf,
    designspace: Arc<DesignSpaceDocument>,
    glyph_names: Arc<HashSet<String>>,
}

fn default_master(designspace: &DesignSpaceDocument) -> Option<(usize, &designspace::Source)> {
    let ds_axes = ir_axes(designspace);
    let axes: HashMap<_, _> = ds_axes.iter().map(|a| (&a.name, a)).collect();

    let default_location = designspace
        .axes
        .iter()
        .map(|a| {
            let converter = &axes.get(&a.name).unwrap().converter;
            (
                a.name.clone(),
                UserCoord::new(a.default).to_design(converter),
            )
        })
        .collect();
    designspace
        .sources
        .iter()
        .enumerate()
        .find(|(_, source)| to_design_location(&source.location) == default_location)
}

fn load_lib_plist(ufo_dir: &Path) -> Result<plist::Dictionary, WorkError> {
    let lib_plist_file = ufo_dir.join("lib.plist");
    if !lib_plist_file.is_file() {
        return Err(WorkError::FileExpected(lib_plist_file));
    }
    plist::Value::from_file(&lib_plist_file)
        .map_err(|e| WorkError::ParseError(lib_plist_file.clone(), format!("{}", e)))?
        .into_dictionary()
        .ok_or_else(|| WorkError::ParseError(lib_plist_file, "Not a dictionary".to_string()))
}

// Per https://github.com/googlefonts/fontmake-rs/pull/43/files#r1044596662
fn glyph_order(
    designspace: &DesignSpaceDocument,
    designspace_dir: &Path,
    glyph_names: &HashSet<String>,
) -> Result<IndexSet<String>, WorkError> {
    // The UFO at the default master *may* elect to specify a glyph order
    // That glyph order *may* deign to overlap with the actual glyph set
    let mut glyph_order = IndexSet::new();
    if let Some((_, source)) = default_master(designspace) {
        let lib_plist = load_lib_plist(&designspace_dir.join(&source.filename))?;
        if let Some(plist::Value::Array(ufo_order)) = lib_plist.get("public.glyphOrder") {
            let mut pending_add: HashSet<_> = glyph_names.iter().map(|s| s.as_str()).collect();
            // Add names from ufo glyph order union glyph_names in ufo glyph order
            ufo_order
                .iter()
                .filter_map(|v| v.as_string().map(|s| s.to_string()))
                .filter(|name| glyph_names.contains(name))
                .for_each(|name| {
                    glyph_order.insert(name.to_string());
                    pending_add.remove(name.as_str());
                });
            // Add anything leftover in sorted order
            let mut pending_add: Vec<_> = pending_add.into_iter().map(|s| s.to_string()).collect();
            pending_add.sort();
            glyph_order.extend(pending_add);
        }
    }
    if glyph_order.is_empty() {
        if glyph_names.contains(".notdef") {
            glyph_order.insert(".notdef".to_string());
        }
        glyph_names
            .iter()
            .filter(|name| name.as_str() != ".notdef")
            .for_each(|name| {
                glyph_order.insert(name.clone());
            });
    }
    Ok(glyph_order)
}

fn ir_axes(designspace: &DesignSpaceDocument) -> Vec<Axis> {
    designspace.axes.iter().map(to_ir_axis).collect()
}

impl Work for StaticMetadataWork {
    fn exec(&self, context: &Context) -> Result<(), WorkError> {
        debug!("Static metadata for {:#?}", self.designspace_file);

        let axes = ir_axes(&self.designspace);

        let glyph_order = glyph_order(
            &self.designspace,
            self.designspace_file.parent().unwrap(),
            &self.glyph_names,
        )?;
        context.set_static_metadata(StaticMetadata::new(axes, glyph_order));
        Ok(())
    }
}

struct GlyphIrWork {
    glyph_name: String,
    glif_files: HashMap<PathBuf, Vec<DesignLocation>>,
}

impl Work for GlyphIrWork {
    fn exec(&self, context: &Context) -> Result<(), WorkError> {
        debug!(
            "Generate glyph IR for {} from {:#?}",
            self.glyph_name, self.glif_files
        );
        let static_metadata = context.get_static_metadata();

        // Migrate glif_files into internal coordinates
        let axes_by_name = static_metadata.axes.iter().map(|a| (&a.name, a)).collect();
        let mut glif_files = HashMap::new();
        for (path, design_locations) in self.glif_files.iter() {
            let normalized_locations: Vec<NormalizedLocation> = design_locations
                .iter()
                .map(|dl| dl.to_normalized(&axes_by_name))
                .collect();
            glif_files.insert(path, normalized_locations);
        }

        let glyph_ir = to_ir_glyph(&self.glyph_name, &glif_files)
            .map_err(|e| WorkError::GlyphIrWorkError(self.glyph_name.clone(), e.to_string()))?;
        context.set_glyph_ir(&self.glyph_name, glyph_ir);
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
        coords::{DesignCoord, DesignLocation},
        source::{Input, Source},
    };
    use indexmap::IndexSet;
    use norad::designspace;

    use crate::toir::to_design_location;

    use super::{default_master, glif_files, glyph_order, DesignSpaceIrSource};

    fn testdata_dir() -> PathBuf {
        let dir = Path::new("../resources/testdata");
        assert!(dir.is_dir());
        dir.to_path_buf()
    }

    fn ufo_dir(filename: &str) -> PathBuf {
        testdata_dir().join(filename)
    }

    fn glifs_for_layer(filename: &str, layer: Option<String>) -> Vec<PathBuf> {
        let mut layer_cache = HashMap::new();
        let source = designspace::Source {
            filename: filename.to_string(),
            layer,
            ..Default::default()
        };
        let ufo_dir = ufo_dir(&source.filename);
        glif_files(&ufo_dir, &mut layer_cache, &source)
            .unwrap()
            .into_values()
            .map(|p| p.strip_prefix(&ufo_dir).unwrap().to_path_buf())
            .collect::<Vec<PathBuf>>()
    }

    #[test]
    pub fn glyphs_from_default_layer() {
        assert_eq!(
            vec![
                PathBuf::from("glyphs/bar.glif"),
                PathBuf::from("glyphs/plus.glif")
            ],
            glifs_for_layer("WghtVar-Regular.ufo", None)
        );
    }

    #[test]
    pub fn glyphs_from_specific_layer() {
        assert_eq!(
            vec![PathBuf::from("glyphs.{600}/bar.glif")],
            glifs_for_layer("WghtVar-Regular.ufo", Some("{600}".to_string()))
        );
    }

    fn test_source() -> (DesignSpaceIrSource, Input) {
        let mut source = DesignSpaceIrSource::new(testdata_dir().join("wght_var.designspace"));
        let input = source.inputs().unwrap();
        (source, input)
    }

    fn add_design_location(
        add_to: &mut HashMap<PathBuf, Vec<DesignLocation>>,
        glif_file: &str,
        axis: &str,
        pos: f32,
    ) {
        let mut loc = DesignLocation::new();
        loc.set_pos(axis, DesignCoord::new(pos));
        add_to
            .entry(testdata_dir().join(glif_file))
            .or_default()
            .push(loc);
    }

    #[test]
    pub fn create_work() {
        let (ir_source, input) = test_source();

        let work = ir_source.create_work_for_one_glyph("bar", &input).unwrap();

        let mut expected_glif_files = HashMap::new();
        add_design_location(
            &mut expected_glif_files,
            "WghtVar-Regular.ufo/glyphs/bar.glif",
            "Weight",
            400.0,
        );
        add_design_location(
            &mut expected_glif_files,
            "WghtVar-Regular.ufo/glyphs.{600}/bar.glif",
            "Weight",
            600.0,
        );
        add_design_location(
            &mut expected_glif_files,
            "WghtVar-Bold.ufo/glyphs/bar.glif",
            "Weight",
            700.0,
        );
        assert_eq!(expected_glif_files, work.glif_files);
    }

    #[test]
    pub fn create_sparse_work() {
        let (ir_source, input) = test_source();

        let work = ir_source.create_work_for_one_glyph("plus", &input).unwrap();

        // Note there is NOT a glyphs.{600} version of plus
        let mut expected_glif_files = HashMap::new();
        add_design_location(
            &mut expected_glif_files,
            "WghtVar-Regular.ufo/glyphs/plus.glif",
            "Weight",
            400.0,
        );
        add_design_location(
            &mut expected_glif_files,
            "WghtVar-Bold.ufo/glyphs/plus.glif",
            "Weight",
            700.0,
        );
        assert_eq!(expected_glif_files, work.glif_files);
    }

    #[test]
    pub fn only_glyphs_present_in_default() {
        let (_, inputs) = test_source();
        // bonus_bar is not present in the default master; should discard
        assert!(!inputs.glyphs.contains_key("bonus_bar"));
    }

    #[test]
    pub fn find_default_master() {
        let (source, _) = test_source();
        let ds = source.load_designspace().unwrap();
        let mut loc = DesignLocation::new();
        loc.set_pos("Weight", DesignCoord::new(400.0));
        assert_eq!(
            loc,
            to_design_location(&default_master(&ds).unwrap().1.location)
        );
    }

    #[test]
    pub fn builds_glyph_order() {
        // Only WghtVar-Regular.ufo has a lib.plist, and it only lists a subset of glyphs
        // Should still work.
        let (source, _) = test_source();
        let ds = source.load_designspace().unwrap();
        let go = glyph_order(
            &ds,
            &source.designspace_dir,
            &HashSet::from([
                "bar".to_string(),
                "plus".to_string(),
                "an-imaginary-one".to_string(),
            ]),
        )
        .unwrap();
        // lib.plist specifies plus, so plus goes first and then the rest in alphabetical order
        let expected: IndexSet<String> = vec!["plus", "an-imaginary-one", "bar"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(expected, go);
    }
}
