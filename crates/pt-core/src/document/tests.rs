use super::*;
use crate::renderer::{Progress, render_progressive};
use crate::toml::{Node, format_value};

struct Quiet;
impl Progress for Quiet {}

fn scenes() -> Vec<PathBuf> {
    let dir = crate::repo_root().join("scenes");
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    v.sort();
    assert!(v.len() >= 10);
    v
}

/// A tiny, single-threaded, deterministic render: the mean of every pixel.
fn render(mut scene: Scene) -> Vec<[u32; 3]> {
    let s = &mut scene.settings;
    let aspect = s.height as f32 / s.width.max(1) as f32;
    s.width = 32;
    s.height = ((32.0 * aspect).round() as u32).max(1);
    (s.spp, s.threads, s.seed, s.noise_threshold, s.time_limit, s.guiding) = (4, 1, 0, 0.0, 0.0, false);
    let (w, h) = (s.width as usize, s.height as usize);
    let px = render_progressive(&scene, &scene.camera, w, h, &Quiet).expect("not cancelled");
    px.iter().map(crate::renderer::Pixel::mean).map(|c| [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]).collect()
}

/// Every scene file opens as a document that is byte-identical to the file, saving it
/// unedited writes nothing, and the document builds the same scene as the loader.
#[test]
fn every_scene_round_trips() {
    for path in scenes() {
        let mut doc = SceneDoc::open(&path).unwrap_or_else(|e| panic!("{e}"));
        for f in &doc.files {
            assert_eq!(f.doc.text(), std::fs::read_to_string(&f.path).unwrap(), "{}", f.path.display());
        }
        assert!(!doc.is_dirty());
        assert_eq!(doc.save().unwrap(), Vec::<PathBuf>::new(), "{}", path.display());
        let direct = loader::load_scene(&path).unwrap();
        let via_doc = doc.to_scene().unwrap();
        assert_eq!(direct.stats(), via_doc.stats(), "{}", path.display());
        assert_eq!(doc.merged().unwrap(), loader::load_document(&path).unwrap());
        assert_eq!(doc.validate().unwrap(), []);
    }
}

fn scalars<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    match &n.kind {
        NodeKind::Scalar(_) => out.push(n),
        NodeKind::Array(items) | NodeKind::TableArray(items) => items.iter().for_each(|c| scalars(c, out)),
        NodeKind::Table(t) => t.entries.iter().for_each(|e| scalars(&e.node, out)),
    }
}

/// Every value of every scene, written again by the writer (`1_280` becomes `1280`,
/// `'x'` becomes `"x"`, `1.0` becomes `1`), reads back as the same scene and renders the
/// same image.
#[test]
fn rewritten_values_render_the_same() {
    let mut rewritten_files = 0;
    for path in scenes() {
        let doc = SceneDoc::open(&path).unwrap();
        let mut texts: Vec<(PathBuf, String)> = Vec::new();
        for f in &doc.files {
            let mut nodes = Vec::new();
            scalars(f.doc.root(), &mut nodes);
            nodes.sort_by_key(|n| std::cmp::Reverse(n.span.start));
            let mut text = f.doc.text().to_owned();
            for n in nodes {
                text.replace_range(n.span.clone(), &format_value(&n.value()));
            }
            rewritten_files += usize::from(text != f.doc.text());
            texts.push((f.path.clone(), text));
        }
        let read = |p: &Path| -> Result<String, String> {
            texts.iter().find(|(q, _)| q == p).map(|(_, t)| t.clone()).ok_or_else(|| loader::read_file(p).unwrap_err())
        };
        let rewritten = SceneDoc::open_from(&path, &read).unwrap();
        assert_eq!(rewritten.merged().unwrap(), doc.merged().unwrap(), "{}", path.display());
        let (a, b) = (render(doc.to_scene().unwrap()), render(rewritten.to_scene().unwrap()));
        assert!(a == b, "{}: renders differ", path.display());
    }
    assert!(rewritten_files > 5, "the writer's formatting differs from the scenes' in only {rewritten_files} files");
}

fn temp_scene(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pt-document-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(d.join("lib")).unwrap();
    for (f, text) in files {
        std::fs::write(d.join(f), text).unwrap();
    }
    d
}

const LIB: &str = "# Shared materials\n[materials.red]\ntype = \"diffuse\"\nalbedo = [0.8, 0.1, 0.1]\n\n\
                   [materials.blue]\nalbedo = [0.1, 0.1, 0.8]\n\n[[objects]]\ntype = \"sphere\"   # from the library\n";
const SCENE: &str = "include = \"lib/mats.toml\"\n\n[render]\nspp = 4  # quick\n\n[materials.blue]\nalbedo = 0.5\n\n\
                     [[objects]]\nname = \"floor\"\ntype = \"quad\"\norigin = [-1, 0, -1]\nu = [2, 0, 0]\nv = [0, 0, 2]\n\
                     material = \"red\"\n";

#[test]
fn tree_follows_the_merge_rules() {
    let d = temp_scene("tree", &[("lib/mats.toml", LIB), ("scene.toml", SCENE)]);
    let doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    assert_eq!(doc.files.len(), 2);
    let t = &doc.tree;
    let labels: Vec<(&str, usize)> = t.objects.iter().map(|o| (o.label.as_str(), o.file)).collect();
    assert_eq!(labels, [("sphere", 1), ("floor", 0)], "included objects come first");
    let mats: Vec<(&str, usize)> = t.materials.iter().map(|m| (m.label.as_str(), m.file)).collect();
    assert_eq!(mats, [("blue", 0), ("red", 1)], "the scene's blue overrides the library's");
    assert_eq!(t.settings, [Setting { section: "render", key: "spp".into(), file: 0 }]);
    assert_eq!(doc.item_table(&t.objects[1]).unwrap()["material"].as_str(), Some("red"));
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn edits_go_to_their_file_and_undo() {
    let d = temp_scene("edit", &[("lib/mats.toml", LIB), ("scene.toml", SCENE)]);
    let mut doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    let red = doc.tree.materials.iter().find(|m| m.label == "red").unwrap().clone();
    let mut albedo = red.path.clone();
    albedo.push("albedo".into());
    let v = Value::Array(vec![Value::Number(0.9), Value::Number(0.2), Value::Number(0.2)]);
    assert!(doc.set(red.file, &albedo, &v).unwrap());
    assert!(!doc.set(red.file, &albedo, &v).unwrap(), "setting an equal value changes nothing");
    let floor = doc.tree.objects[1].clone();
    let mut radius = floor.path.clone();
    radius.push("name".into());
    doc.set(floor.file, &radius, &Value::String("ground".into())).unwrap();
    assert_eq!(doc.tree.objects[1].label, "ground");
    doc.push_table(0, &["objects".into()], &[("type".into(), Value::String("sphere".into()))]).unwrap();
    assert_eq!(doc.tree.objects.len(), 3);
    let scene = doc.to_scene().unwrap();
    assert!(scene.stats().contains("2 shapes") || scene.num_prims() >= 4, "{}", scene.stats());

    // Only the changed files are written; the rest of each file is untouched.
    let written = doc.save().unwrap();
    assert_eq!(written.len(), 2);
    let lib = std::fs::read_to_string(d.join("lib/mats.toml")).unwrap();
    assert_eq!(lib, LIB.replace("albedo = [0.8, 0.1, 0.1]", "albedo = [0.9, 0.2, 0.2]"));
    let scene_text = std::fs::read_to_string(d.join("scene.toml")).unwrap();
    assert!(scene_text.starts_with(&SCENE.replace("\"floor\"", "\"ground\"")), "{scene_text}");
    assert!(scene_text.ends_with("material = \"red\"\n\n[[objects]]\ntype = \"sphere\"\n"), "{scene_text}");
    assert!(!doc.is_dirty());

    // Undo everything: back to the original texts (and dirty, since that is not on disk).
    while doc.undo().unwrap() {}
    assert_eq!(doc.files[0].doc.text(), SCENE);
    assert_eq!(doc.files[1].doc.text(), LIB);
    assert!(doc.is_dirty());
    assert!(doc.redo().unwrap());
    assert!(doc.files[1].doc.text().contains("[0.9, 0.2, 0.2]"));
    // A new edit drops what could be redone.
    doc.remove(0, &["render".into()]).unwrap();
    assert!(!doc.history().can_redo());
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn drags_are_one_undo_step() {
    let d = temp_scene("drag", &[("lib/mats.toml", LIB), ("scene.toml", SCENE)]);
    let mut doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    let spp: Vec<Seg> = vec!["render".into(), "spp".into()];
    for v in 5..=40 {
        doc.set_merging(0, &spp, &Value::Number(f64::from(v)), "spp-slider").unwrap();
    }
    doc.seal();
    doc.set_merging(0, &spp, &Value::Number(64.0), "spp-slider").unwrap();
    assert!(doc.files[0].doc.text().contains("spp = 64  # quick"));
    assert!(doc.undo().unwrap());
    assert!(doc.files[0].doc.text().contains("spp = 40  # quick"));
    assert!(doc.undo().unwrap());
    assert_eq!(doc.files[0].doc.text(), SCENE);
    assert!(!doc.history().can_undo());
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn renames_update_references_in_one_undo_step() {
    let d = temp_scene("rename", &[("lib/mats.toml", LIB), ("scene.toml", SCENE)]);
    let mut doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    let red = doc.tree.materials.iter().find(|m| m.label == "red").unwrap().clone();
    assert!(doc.rename(&red, "crimson").unwrap());
    assert!(doc.files[1].doc.text().contains("[materials.crimson]\ntype = \"diffuse\""));
    assert!(doc.files[0].doc.text().contains("material = \"crimson\"\n"), "the reference in the other file");
    assert!(doc.tree.materials.iter().any(|m| m.label == "crimson"));
    doc.to_scene().unwrap();
    let blue = doc.tree.materials.iter().find(|m| m.label == "blue").unwrap().clone();
    assert!(doc.rename(&blue, "crimson").unwrap_err().contains("already"));
    assert!(doc.undo().unwrap());
    assert_eq!((doc.files[0].doc.text(), doc.files[1].doc.text()), (SCENE, LIB));
    // Objects and lights rename by their `name`.
    let floor = doc.tree.objects[1].clone();
    doc.rename(&floor, "ground").unwrap();
    assert_eq!(doc.tree.objects[1].label, "ground");
    doc.rename(&doc.tree.objects[1].clone(), "").unwrap();
    assert_eq!(doc.tree.objects[1].label, "quad");
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn objects_move_within_their_file() {
    let scene = format!("{SCENE}\n[[objects]]\ntype = \"sphere\"\nname = \"ball\"\n");
    let d = temp_scene("move", &[("lib/mats.toml", LIB), ("scene.toml", &scene)]);
    let mut doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    let labels = |doc: &SceneDoc| doc.tree.objects.iter().map(|o| o.label.clone()).collect::<Vec<_>>();
    assert_eq!(labels(&doc), ["sphere", "floor", "ball"]);
    let ball = doc.tree.objects[2].clone();
    assert!(doc.move_item(&ball, 0).unwrap());
    assert_eq!(labels(&doc), ["sphere", "ball", "floor"], "first among the scene file's own objects");
    doc.to_scene().unwrap();
    assert!(doc.undo().unwrap());
    assert_eq!(doc.files[0].doc.text(), scene);
    std::fs::remove_dir_all(d).ok();
}

/// Duplicating copies an entry's text, comments and all, after the last entry; changes
/// made on disk come in as an undoable edit.
#[test]
fn blocks_copy_and_disk_changes_reload() {
    let scene = "[camera]\nfov = 30\n\n# the ball\n[[objects]]\ntype = \"sphere\"  # round\n\n\
                 [[objects]]\ntype = \"quad\"\norigin = [0, 0, 0]\nu = [1, 0, 0]\nv = [0, 1, 0]\n\n\
                 [materials.red]\nalbedo = [1, 0, 0]\n";
    let d = temp_scene("blocks", &[("scene.toml", scene)]);
    let mut doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    let ball = doc.files[0].doc.block_text(&["objects".into(), Seg::Index(0)]).unwrap();
    assert_eq!(ball, "# the ball\n[[objects]]\ntype = \"sphere\"  # round");
    assert!(doc.insert_block(0, &["objects".into()], &ball).unwrap());
    assert!(
        doc.files[0]
            .doc
            .text()
            .contains("v = [0, 1, 0]\n\n# the ball\n[[objects]]\ntype = \"sphere\"  # round\n\n[materials.red]"),
        "{}",
        doc.files[0].doc.text()
    );
    assert_eq!(doc.tree.objects.len(), 3);
    let red = doc.files[0].doc.block_text(&["materials".into(), "red".into()]).unwrap();
    assert_eq!(red, "[materials.red]\nalbedo = [1, 0, 0]");
    assert!(doc.files[0].doc.block_text(&["camera".into(), "fov".into()]).is_none());
    doc.to_scene().unwrap();
    assert!(doc.undo().unwrap());
    assert_eq!(doc.files[0].doc.text(), scene);

    std::fs::write(d.join("scene.toml"), scene.replace("fov = 30", "fov = 50")).unwrap();
    assert!(doc.reload_file(0).unwrap());
    assert!(!doc.is_dirty() && doc.files[0].doc.text().contains("fov = 50"));
    assert!(doc.undo().unwrap());
    assert!(doc.is_dirty(), "undoing the change on disk is an edit");
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn save_as_stays_in_the_folder() {
    let d = temp_scene("saveas", &[("lib/mats.toml", LIB), ("scene.toml", SCENE)]);
    let mut doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    assert!(doc.save_as(&d.join("lib/elsewhere.toml")).is_err());
    let written = doc.save_as(&d.join("copy.toml")).unwrap();
    assert_eq!(written, [d.join("copy.toml")]);
    assert_eq!(std::fs::read_to_string(d.join("copy.toml")).unwrap(), SCENE);
    assert_eq!(doc.path(), d.join("copy.toml"));
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn include_edits_load_files_and_cycles_are_refused() {
    let d = temp_scene(
        "include",
        &[("lib/mats.toml", LIB), ("scene.toml", SCENE), ("lib/more.toml", "[materials.green]\n")],
    );
    let mut doc = SceneDoc::open(&d.join("scene.toml")).unwrap();
    let inc = Value::Array(vec![Value::String("lib/mats.toml".into()), Value::String("lib/more.toml".into())]);
    doc.set(0, &["include".into()], &inc).unwrap();
    assert_eq!(doc.files.len(), 3);
    assert!(doc.tree.materials.iter().any(|m| m.label == "green" && m.file == 2));
    let e = doc.set(1, &["include".into()], &Value::String("../scene.toml".into())).unwrap_err();
    assert!(e.contains("cycle"), "{e}");
    assert_eq!(doc.files[1].doc.text(), LIB, "the refused edit is reverted");
    assert!(doc.undo().unwrap());
    assert_eq!(doc.files[0].doc.text(), SCENE);
    std::fs::remove_dir_all(d).ok();
}
