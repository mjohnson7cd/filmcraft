//! `file.import` of a file the project already has (#356): no second item, the existing one under
//! `duplicates` (apart from `errors`), whatever path leads to the file: a symlink, a hard link,
//! `..`, another letter case on Windows. Image sequences and the `bin` parameter included.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use filmcraft_media::MediaKind;
use filmcraft_project::{BinId, ItemId, Project};
use serde_json::{Value, json};

use crate::media_test_util::tmp_dir;
use crate::{Event, FileIdentity, FsServices, Services, Session};

/// A stereo 16-bit WAV with `frames` sample frames: every `frames` is another file size.
fn wav(frames: usize) -> Vec<u8> {
    filmcraft_media::wav::write_wav16(&vec![0.25; frames * 2], 2, 48_000)
}

fn write_wav(path: &Path, frames: usize) -> String {
    std::fs::write(path, wav(frames)).unwrap();
    path.to_string_lossy().to_string()
}

/// Numbered PNG stills `prefix0001.png`… in `dir`; returns the paths.
fn write_frames(dir: &Path, prefix: &str, numbers: &[u32]) -> Vec<String> {
    numbers
        .iter()
        .map(|&n| {
            let p = dir.join(format!("{prefix}{n:04}.png"));
            image::RgbaImage::from_pixel(32, 18, image::Rgba([(10 * n) as u8, 99, 7, 255])).save(&p).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect()
}

fn import(s: &mut Session, p: Value) -> Value {
    s.execute("file.import", p).unwrap()
}

fn items(r: &Value) -> Vec<ItemId> {
    r["items"].as_array().unwrap().iter().map(|v| ItemId(v.as_u64().unwrap())).collect()
}

/// The `(item, moved)` of every duplicate a result reports.
fn duplicates(r: &Value) -> Vec<(ItemId, bool)> {
    r["duplicates"].as_array().unwrap_or_else(|| panic!("{r}")).iter().map(|d| (ItemId(d["item"].as_u64().unwrap()), d["moved"].as_bool().unwrap())).collect()
}

fn media_count(p: &Project) -> usize {
    p.items.values().filter(|i| i.as_media().is_some()).count()
}

#[test]
fn the_same_path_again_is_reported_not_added() {
    let dir = tmp_dir("dup-same");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let r = import(&mut s, json!({"paths": [a]}));
    let id = items(&r)[0];
    assert_eq!(r["duplicates"], json!([]), "the field is always there: {r}");
    let undo = s.history.undo.len();
    s.drain_events();

    let r = import(&mut s, json!({"paths": [a]}));
    assert_eq!(r["items"], json!([]));
    assert_eq!(r["duplicates"], json!([{"path": a, "item": id.0, "moved": false}]));
    assert_eq!(r["errors"], json!([]), "a duplicate is not an error: {r}");
    assert_eq!(media_count(&s.project), 1);
    assert_eq!(s.history.undo.len(), undo, "nothing to undo");
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::Toast { message, error: false } if message == "Already in the project: a.wav")));

    // a copy is another file, however alike
    let b = write_wav(&dir.join("b.wav"), 4800);
    let r = import(&mut s, json!({"paths": [b]}));
    assert_eq!(items(&r).len(), 1, "{r}");
    assert_eq!(duplicates(&r), []);
    assert_eq!(media_count(&s.project), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_listed_twice_in_one_import_is_added_once() {
    let dir = tmp_dir("dup-once");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let b = write_wav(&dir.join("b.wav"), 2400);
    let mut s = Session::default();
    let r = import(&mut s, json!({"paths": [a, b, a]}));
    let ids = items(&r);
    assert_eq!(ids.len(), 2, "{r}");
    assert_eq!(duplicates(&r), [(ids[0], false)]);
    assert_eq!(media_count(&s.project), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn duplicates_and_errors_are_reported_apart() {
    let dir = tmp_dir("dup-errors");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let missing = dir.join("missing.wav").to_string_lossy().to_string();
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];

    // a duplicate beside a failure: the command succeeds and says both, each in its place
    let r = import(&mut s, json!({"paths": [a, missing]}));
    assert_eq!(duplicates(&r), [(id, false)]);
    let errors: Vec<&str> = r["errors"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(errors.len(), 1, "{r}");
    assert!(errors[0].starts_with(&missing), "{r}");
    assert!(!r["errors"].to_string().contains("a.wav"), "{r}");

    // nothing but failures is still an error
    let e = s.execute("file.import", json!({"paths": [missing]})).unwrap_err().to_string();
    assert!(e.contains("missing.wav"), "{e}");
    // and a file that is missing now is not a duplicate of the item it once was
    std::fs::remove_file(&a).unwrap();
    assert!(s.execute("file.import", json!({"paths": [a]})).is_err());
    assert_eq!(media_count(&s.project), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_duplicate_asked_into_another_bin_moves_there() {
    let dir = tmp_dir("dup-bin");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    let root = s.project.root.id;
    let bin = BinId(s.execute("file.newBin", json!({"name": "Sound"})).unwrap()["bin"].as_u64().unwrap());
    assert_eq!(s.project.root.parent_of(id), Some(root));

    // no bin asked for: it stays where it is
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [a]}))), [(id, false)]);
    // into the bin: the one item moves (no copy), and undo puts it back
    let r = import(&mut s, json!({"paths": [a], "bin": bin.0}));
    assert_eq!(duplicates(&r), [(id, true)]);
    assert_eq!(r["items"], json!([]));
    assert_eq!(s.project.root.parent_of(id), Some(bin));
    assert_eq!(media_count(&s.project), 1);
    // already there
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [a], "bin": bin.0}))), [(id, false)]);
    // a plain import later does not pull it out of its bin
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [a]}))), [(id, false)]);
    assert_eq!(s.project.root.parent_of(id), Some(bin));
    // a bin that does not exist moves nothing
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [a], "bin": 999_999}))), [(id, false)]);
    assert_eq!(s.project.root.parent_of(id), Some(bin));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.root.parent_of(id), Some(root));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_item_made_offline_or_changed_on_disk_does_not_hide_the_file() {
    let dir = tmp_dir("dup-offline");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    // the file was replaced by a longer one: new media
    write_wav(Path::new(&a), 9600);
    let r = import(&mut s, json!({"paths": [a]}));
    let second = items(&r)[0];
    assert_eq!(duplicates(&r), []);
    // both made offline on purpose: importing brings the file in again
    s.edit("offline", |p, _| {
        for i in [id, second] {
            if let Some(m) = p.item_mut(i).and_then(|it| it.as_media_mut()) {
                m.offline = true;
            }
        }
        Ok(())
    })
    .unwrap();
    let r = import(&mut s, json!({"paths": [a]}));
    assert_eq!(items(&r).len(), 1, "{r}");
    assert_eq!(duplicates(&r), []);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_image_sequence_and_its_first_still_are_two_items() {
    let dir = tmp_dir("dup-seq");
    let f = write_frames(&dir, "shot.", &[1, 2, 3]);
    let mut s = Session::default();
    let seq = items(&import(&mut s, json!({"paths": [f[0]], "imageSequence": true})))[0];
    assert_eq!(s.project.item(seq).unwrap().as_media().unwrap().info.kind, MediaKind::ImageSequence);
    // the sequence again
    let r = import(&mut s, json!({"paths": [f[0]], "imageSequence": true}));
    assert_eq!(duplicates(&r), [(seq, false)]);
    assert!(r.get("imageSequences").is_none(), "{r}");
    let r = s.execute("file.importImageSequence", json!({"path": f[0]})).unwrap();
    assert_eq!(duplicates(&r), [(seq, false)]);
    // its first frame as a still is another item; then that still again is a duplicate of the still
    let still = items(&import(&mut s, json!({"paths": [f[0]]})))[0];
    assert_eq!(s.project.item(still).unwrap().as_media().unwrap().info.kind, MediaKind::Still);
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [f[0]]}))), [(still, false)]);
    // the sequence from its second frame on is other media
    let r = import(&mut s, json!({"paths": [f[1]], "imageSequence": true}));
    assert_eq!(items(&r).len(), 1, "{r}");
    // Settings ▸ Media ▸ Import image sequences: a detected sequence is the same sequence
    s.execute("prefs.set", json!({"key": "media.importImageSequences", "value": true})).unwrap();
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [f[0]]}))), [(seq, false)]);
    assert_eq!(media_count(&s.project), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn unix_links_and_dotted_paths_lead_to_the_same_file() {
    let dir = tmp_dir("dup-unix");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];

    let sym = dir.join("sym.wav");
    std::os::unix::fs::symlink(&a, &sym).unwrap();
    let hard = dir.join("sub").join("hard.wav");
    std::fs::hard_link(&a, &hard).unwrap();
    let dotted = dir.join("sub").join("..").join("a.wav");
    let linked_dir = dir.join("linked");
    std::os::unix::fs::symlink(&dir, &linked_dir).unwrap();
    let through_dir = linked_dir.join("a.wav");
    for p in [&sym, &hard, &dotted, &through_dir] {
        let r = import(&mut s, json!({"paths": [p.to_string_lossy()]}));
        assert_eq!(duplicates(&r), [(id, false)], "{}: {r}", p.display());
        assert_eq!(r["duplicates"][0]["path"], json!(p.to_string_lossy()), "the path as it was given");
    }
    // a symlink that leads nowhere is an error, not a duplicate
    let dangling = dir.join("dangling.wav");
    std::os::unix::fs::symlink(dir.join("gone.wav"), &dangling).unwrap();
    assert!(s.execute("file.import", json!({"paths": [dangling.to_string_lossy()]})).is_err());
    assert_eq!(media_count(&s.project), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A hard link to a sequence's first frame is the same file but not the same sequence: the frames
/// beside it are others (or none), in its own directory or under its own name.
#[cfg(unix)]
#[test]
fn unix_hard_linked_frames_start_their_own_sequences() {
    let dir = tmp_dir("dup-seq-links");
    let (one, two) = (dir.join("one"), dir.join("two"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let f = write_frames(&one, "shot.", &[1, 2, 3]);
    let mut s = Session::default();
    let seq = items(&import(&mut s, json!({"paths": [f[0]], "imageSequence": true})))[0];
    let frames = |s: &Session, id: ItemId| {
        let m = s.project.item(id).unwrap().as_media().unwrap();
        m.info.duration.0 / m.info.frame_rate().tick_of(1).0
    };
    assert_eq!(frames(&s, seq), 3);

    // in another directory, with other frames beside it
    let other = two.join("shot.0001.png");
    std::fs::hard_link(&f[0], &other).unwrap();
    write_frames(&two, "shot.", &[2, 3, 4, 5]);
    let r = import(&mut s, json!({"paths": [other.to_string_lossy()], "imageSequence": true}));
    assert_eq!(duplicates(&r), [], "{r}");
    assert_eq!(frames(&s, items(&r)[0]), 5);
    // under another name in the same directory
    let renamed = one.join("alt.0001.png");
    std::fs::hard_link(&f[0], &renamed).unwrap();
    let r = import(&mut s, json!({"paths": [renamed.to_string_lossy()], "imageSequence": true}));
    assert_eq!(duplicates(&r), [], "{r}");
    assert_eq!(frames(&s, items(&r)[0]), 1);
    // identical frames stored once (frame 7 is a hard link to frame 6): 6… and 7… differ
    let held = write_frames(&one, "hold.", &[6]);
    let next = one.join("hold.0007.png");
    std::fs::hard_link(&held[0], &next).unwrap();
    let a = items(&import(&mut s, json!({"paths": [held[0]], "imageSequence": true})))[0];
    let r = import(&mut s, json!({"paths": [next.to_string_lossy()], "imageSequence": true}));
    assert_eq!(duplicates(&r), [], "{r}");
    assert_eq!((frames(&s, a), frames(&s, items(&r)[0])), (2, 1));

    // the same sequence through a symlinked directory is the same sequence
    let linked = dir.join("linked");
    std::os::unix::fs::symlink(&one, &linked).unwrap();
    let r = import(&mut s, json!({"paths": [linked.join("shot.0001.png").to_string_lossy()], "imageSequence": true}));
    assert_eq!(duplicates(&r), [(seq, false)], "{r}");
    // as single stills, the hard links are one file
    let still = items(&import(&mut s, json!({"paths": [f[0]]})))[0];
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [other.to_string_lossy()]}))), [(still, false)]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The real thing on Windows: volume serial number + file index see through a hard link, another
/// letter case and `..`.
#[cfg(windows)]
#[test]
fn windows_links_letter_case_and_dotted_paths_lead_to_the_same_file() {
    let dir = tmp_dir("dup-windows");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let a = write_wav(&dir.join("Clip.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    let hard = dir.join("sub").join("hard.wav");
    std::fs::hard_link(&a, &hard).unwrap();
    let dotted = dir.join("sub").join("..").join("Clip.wav");
    for p in [hard.to_string_lossy().to_string(), dotted.to_string_lossy().to_string(), a.to_uppercase(), a.replace('\\', "/")] {
        let r = import(&mut s, json!({"paths": [p]}));
        assert_eq!(duplicates(&r), [(id, false)], "{p}: {r}");
    }
    let copy = write_wav(&dir.join("copy.wav"), 4800);
    assert_eq!(items(&import(&mut s, json!({"paths": [copy]}))).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(unix, windows))]
#[test]
fn native_file_identity_is_the_file_not_the_path() {
    let dir = tmp_dir("dup-identity");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let b = write_wav(&dir.join("b.wav"), 4800);
    let hard = dir.join("hard.wav");
    std::fs::hard_link(&a, &hard).unwrap();
    let ia = FsServices.file_identity(&a).unwrap();
    assert_eq!(ia.size, std::fs::metadata(&a).unwrap().len());
    assert_eq!(FsServices.file_identity(&hard.to_string_lossy()), Some(ia));
    let ib = FsServices.file_identity(&b).unwrap();
    assert_eq!((ib.size, ib.volume), (ia.size, ia.volume));
    assert_ne!(ib.index, ia.index);
    // a directory has one too (size 0); a missing path has none
    assert_eq!(FsServices.file_identity(&dir.to_string_lossy()).unwrap().size, 0);
    assert_eq!(FsServices.file_identity(&dir.join("none.wav").to_string_lossy()), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An in-memory host whose files carry the identity a filesystem would report (or none), and
/// which counts the identity lookups per path. Paths are opaque keys, so Windows spellings work on
/// every platform.
#[derive(Default)]
struct IdFs {
    files: Mutex<BTreeMap<String, (Vec<u8>, Option<(u64, u128)>)>>,
    lookups: Mutex<BTreeMap<String, usize>>,
}

impl IdFs {
    fn add(&self, path: &str, bytes: Vec<u8>, id: Option<(u64, u128)>) {
        self.files.lock().unwrap().insert(path.to_string(), (bytes, id));
    }
    fn lookups(&self, path: &str) -> usize {
        self.lookups.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

impl Services for IdFs {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.files.lock().unwrap().get(path).map(|f| f.0.clone()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        self.add(path, data.to_vec(), None);
        Ok(())
    }
    fn file_identity(&self, path: &str) -> Option<FileIdentity> {
        *self.lookups.lock().unwrap().entry(path.to_string()).or_default() += 1;
        let files = self.files.lock().unwrap();
        let (bytes, id) = files.get(path)?;
        id.map(|(volume, index)| FileIdentity { size: bytes.len() as u64, volume, index })
    }
}

/// What Windows reports (one volume serial number + file index for every spelling of a path),
/// and what the import does with it.
#[test]
fn windows_spellings_of_one_file_are_one_item() {
    const VOLUME: u64 = 0x9C1F_33A2;
    let fs = Arc::new(IdFs::default());
    for p in [r"C:\Media\Clip.wav", r"c:\media\CLIP.WAV", r"C:\Media\Sub\..\Clip.wav", r"\\?\C:\Media\Clip.wav", "C:/Media/Clip.wav", r"D:\Links\hardlink.wav"]
    {
        // (a hard link on the same volume; a drive letter is only where the volume is mounted)
        fs.add(p, wav(4800), Some((VOLUME, 0x0005_0000_0000_1A2B)));
    }
    fs.add(r"C:\Media\Other.wav", wav(4800), Some((VOLUME, 0x0005_0000_0000_1A2C)));
    // the same file index on another volume is another file
    fs.add(r"E:\Media\Clip.wav", wav(4800), Some((0x1111_2222, 0x0005_0000_0000_1A2B)));
    // a 128-bit ReFS file id that differs only above bit 64
    fs.add(r"R:\Clip.wav", wav(4800), Some((VOLUME, (1u128 << 64) | 0x0005_0000_0000_1A2B)));
    let mut s = Session::new(fs.clone());
    let id = items(&import(&mut s, json!({"paths": [r"C:\Media\Clip.wav"]})))[0];
    s.drain_events();
    for p in [r"c:\media\CLIP.WAV", r"C:\Media\Sub\..\Clip.wav", r"\\?\C:\Media\Clip.wav", "C:/Media/Clip.wav", r"D:\Links\hardlink.wav"] {
        let r = import(&mut s, json!({"paths": [p]}));
        assert_eq!(r["duplicates"], json!([{"path": p, "item": id.0, "moved": false}]), "{p}");
        assert_eq!(r["items"], json!([]), "{p}");
    }
    // the toast names the file, not its Windows path
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::Toast { message, .. } if message == "Already in the project: hardlink.wav")));
    for p in [r"C:\Media\Other.wav", r"E:\Media\Clip.wav", r"R:\Clip.wav"] {
        let r = import(&mut s, json!({"paths": [p]}));
        assert_eq!((items(&r).len(), duplicates(&r).len()), (1, 0), "{p}: {r}");
    }
    assert_eq!(media_count(&s.project), 4);
}

/// A network share or FUSE mount that gives every file the same index: the size keeps different
/// files apart, and a filesystem that reports index 0 is not believed at all.
#[test]
fn identities_that_are_not_unique_do_not_merge_different_files() {
    let fs = Arc::new(IdFs::default());
    fs.add("/mnt/share/a.wav", wav(4800), Some((7, 1)));
    fs.add("/mnt/share/b.wav", wav(2400), Some((7, 1)));
    let mut s = Session::new(fs.clone());
    let r = import(&mut s, json!({"paths": ["/mnt/share/a.wav", "/mnt/share/b.wav"]}));
    let ids = items(&r);
    assert_eq!((ids.len(), duplicates(&r).len()), (2, 0), "{r}");
    let r = import(&mut s, json!({"paths": ["/mnt/share/b.wav"]}));
    assert_eq!(duplicates(&r), [(ids[1], false)]);
    assert_eq!(media_count(&s.project), 2);

    // FsServices drops an index of 0 (see `native_file_identity…`); such a host compares paths
    let fs = Arc::new(IdFs::default());
    fs.add("/fuse/a.wav", wav(4800), None);
    fs.add("/fuse/b.wav", wav(4800), None);
    let mut s = Session::new(fs);
    let r = import(&mut s, json!({"paths": ["/fuse/a.wav", "/fuse/b.wav"]}));
    assert_eq!((items(&r).len(), duplicates(&r).len()), (2, 0), "{r}");
    assert_eq!(duplicates(&import(&mut s, json!({"paths": ["/fuse/a.wav"]}))).len(), 1);
}

/// A host without file identities (the web): the path as written is all there is.
#[test]
fn without_identities_paths_are_compared_as_written() {
    let fs = Arc::new(IdFs::default());
    fs.add("/dropped/a.wav", wav(4800), None);
    fs.add("/dropped/A.wav", wav(4800), None);
    let mut s = Session::new(fs);
    let id = items(&import(&mut s, json!({"paths": ["/dropped/a.wav"]})))[0];
    assert_eq!(duplicates(&import(&mut s, json!({"paths": ["/dropped/a.wav"]}))), [(id, false)]);
    let r = import(&mut s, json!({"paths": ["/dropped/A.wav"]}));
    assert_eq!((items(&r).len(), duplicates(&r).len()), (1, 0), "{r}");
}

/// One lookup table per command, and only project files of a matching size are looked up on
/// disk: importing into a large project does not `stat` every item.
#[test]
fn only_files_of_a_matching_size_are_looked_up() {
    let fs = Arc::new(IdFs::default());
    let old: Vec<String> = (0..40).map(|i| format!("/media/old{i:02}.wav")).collect();
    for (i, p) in old.iter().enumerate() {
        fs.add(p, wav(1000 + i * 10), Some((1, 100 + i as u128)));
    }
    // two new files; `same.wav` is as large as old07.wav without being it
    fs.add("/media/new.wav", wav(5000), Some((1, 900)));
    fs.add("/media/same.wav", wav(1070), Some((1, 901)));
    fs.add("/media/link-to-old03.wav", wav(1030), Some((1, 103)));
    let mut s = Session::new(fs.clone());
    assert_eq!(items(&import(&mut s, json!({"paths": old}))).len(), 40);
    let before: Vec<usize> = old.iter().map(|p| fs.lookups(p)).collect();

    let r = import(&mut s, json!({"paths": ["/media/new.wav", "/media/same.wav", "/media/link-to-old03.wav", "/media/new.wav"]}));
    assert_eq!(items(&r).len(), 2, "{r}");
    assert_eq!(duplicates(&r).len(), 2, "the link, and new.wav listed twice: {r}");
    let looked_up: Vec<&str> = old.iter().zip(&before).filter(|(p, n)| fs.lookups(p) > **n).map(|(p, _)| p.as_str()).collect();
    assert_eq!(looked_up, ["/media/old03.wav", "/media/old07.wav"]);
    assert_eq!(fs.lookups("/media/old03.wav"), before[3] + 1, "once per command, not once per path");
    // each incoming path is looked up once
    assert_eq!(fs.lookups("/media/same.wav"), 1);
}
