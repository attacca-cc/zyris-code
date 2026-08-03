//! 편집을 되돌린다.
//!
//! `code_edit`이 디스크를 바꾸는데 앱 안에 되돌릴 길이 없었다. git이 없는 디렉터리면
//! 그대로 끝이다. 승인 게이트가 "실행 전"을 지키니 **"실행 후"도 하나 있어야 짝이 맞는다.**
//!
//! **사용자 리포에는 아무것도 만들지 않는다.** `.zyris-code/undo/`에 두면 디렉터리가
//! 생기고, `.gitignore`에 넣어 주지 않는 한 커밋에 섞인다. 남의 리포를 더럽히지 않는다.
//!
//! ```text
//! ~/.cache/zyris-code/undo/home-ruma-zyris-code/
//!     log.jsonl          한 줄에 되돌림 하나, 오래된 것부터
//!     000001-app.rs.bak  바뀌기 전 내용 그대로
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// 들고 있을 최대 개수. 넘으면 오래된 것부터 지운다.
const KEEP: usize = 200;
/// 디렉터리 이름의 최대 길이. 대부분의 파일시스템이 255바이트에서 막는다.
const NAME_LIMIT: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    /// 유닉스 초. 사람에게 보여줄 때만 쓴다.
    at: u64,
    /// 바뀐 파일의 절대경로.
    path: String,
    /// 백업 파일의 이름. 디렉터리 안에서의 이름이다.
    backup: String,
    /// **그 파일이 원래 있었는가.** 없었으면 되돌린다는 것은 지운다는 뜻이다 —
    /// 빈 파일로 되돌리면 있지도 않던 파일이 남는다.
    existed: bool,
}

/// 손댄 파일 하나. 여러 번 고쳤어도 한 줄이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub path: PathBuf,
    /// 몇 번 고쳤는가.
    pub edits: usize,
    /// 없던 파일을 만든 것인가.
    pub created: bool,
    pub added: u32,
    pub removed: u32,
}

/// 한 작업 디렉터리의 되돌림 기록.
#[derive(Clone)]
pub struct Undo(Arc<Inner>);

struct Inner {
    dir: PathBuf,
    /// 파일 조작 전체를 한 줄로 세운다. 편집은 도구 호출마다 오고 그것들은 서로 다른
    /// 태스크다 — 안 세우면 로그 두 줄이 겹쳐 쓰인다.
    lock: Mutex<()>,
}

impl Undo {
    /// 이 작업 디렉터리의 기록을 연다. 디렉터리는 처음 쓸 때 만든다.
    pub fn for_dir(cwd: &Path) -> Undo {
        Undo(Arc::new(Inner { dir: home().join("undo").join(slug(cwd)), lock: Mutex::new(()) }))
    }

    /// 쓰기 **직전에** 부른다.
    ///
    /// **실패해도 편집을 막지 않는다.** 안전망이 없다고 일을 막으면 고칠 수 없는 파일이
    /// 생긴다 — `Preview`가 실패해도 승인을 막지 않는 것과 같은 판단이다. 대신 그 편집은
    /// 기록에 남지 않고, `/undo`는 그 앞의 것을 되돌린다.
    pub fn snapshot(&self, path: &Path) {
        if let Err(e) = self.try_snapshot(path) {
            tracing::warn!(path = %path.display(), "되돌림 기록을 남기지 못했다: {e}");
        }
    }

    fn try_snapshot(&self, path: &Path) -> std::io::Result<()> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(&self.0.dir)?;

        let before = std::fs::read(path);
        let existed = before.is_ok();
        let mut log = self.read_log();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // 이름이 겹쳐도 덮어쓰지 않도록 번호를 앞에 붙인다.
        let backup = format!("{:06}-{}.bak", self.next_number(&log), name);
        std::fs::write(self.0.dir.join(&backup), before.unwrap_or_default())?;

        log.push(Record {
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            backup,
            existed,
        });
        self.trim(&mut log);
        self.write_log(&log)
    }

    /// 마지막 편집 하나를 되돌린다. 되돌린 파일의 경로를 준다.
    pub fn revert_last(&self) -> Result<PathBuf, String> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut log = self.read_log();
        let Some(last) = log.pop() else {
            return Err("되돌릴 편집이 없습니다.".into());
        };
        let path = PathBuf::from(&last.path);
        let backup = self.0.dir.join(&last.backup);

        let outcome = if last.existed {
            std::fs::read(&backup).and_then(|content| std::fs::write(&path, content))
        } else {
            // 없던 파일을 만든 편집이었다. 되돌린다는 것은 지운다는 뜻이다.
            // 사람이 이미 지웠으면 그것도 원하는 상태이므로 성공으로 친다.
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        };
        // **성공하든 실패하든 이 줄은 뺀다.** 안 빼면 `/undo`를 누를 때마다 같은 것에
        // 걸려 그 앞의 편집을 영영 되돌릴 수 없다.
        let _ = std::fs::remove_file(&backup);
        let _ = self.write_log(&log);
        outcome.map(|()| path).map_err(|e| format!("되돌리지 못했습니다: {e}"))
    }

    /// 되돌릴 것이 있는가.
    pub fn is_empty(&self) -> bool {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read_log().is_empty()
    }

    /// 이 디렉터리에서 바꾼 파일들. **최근에 손댄 것이 앞이다.**
    ///
    /// 파일마다 **가장 오래된 기록**의 백업이 "손대기 전"이고, 그것과 지금 디스크에 있는
    /// 것을 견준다. 편집 하나하나가 아니라 통틀어 얼마나 달라졌는지가 알고 싶은 것이다 —
    /// 같은 파일을 다섯 번 고쳤다는 사실보다 결국 몇 줄이 바뀌었는지가 먼저다.
    ///
    /// 기록은 앱을 껐다 켜도 남으므로 **이번 실행의 것만은 아니다.** `/undo`가 거슬러
    /// 올라가는 범위와 같다 — 둘이 갈라지면 되돌릴 수 있는 것과 보이는 것이 어긋난다.
    pub fn changed(&self) -> Vec<Changed> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        let log = self.read_log();

        // 오래된 것부터 훑으면서 파일마다 처음 것을 붙잡고, 순서는 마지막에 손댄 차례로 둔다.
        let mut order: Vec<&str> = Vec::new();
        let mut first: std::collections::HashMap<&str, &Record> = std::collections::HashMap::new();
        let mut edits: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for record in &log {
            first.entry(&record.path).or_insert(record);
            *edits.entry(&record.path).or_default() += 1;
            order.retain(|p| *p != record.path);
            order.push(&record.path);
        }
        order.reverse();

        order
            .into_iter()
            .map(|path| {
                let record = first[path];
                let before =
                    std::fs::read_to_string(self.0.dir.join(&record.backup)).unwrap_or_default();
                // 지금 없으면 지워진 것이다. 빈 글로 견주면 전부 삭제로 나온다 — 맞다.
                let now = std::fs::read_to_string(path).unwrap_or_default();
                let d = crate::tools::diff::diff(&before, &now, path);
                Changed {
                    path: PathBuf::from(path),
                    edits: edits[path],
                    created: !record.existed,
                    added: d.added,
                    removed: d.removed,
                }
            })
            .collect()
    }

    fn log_path(&self) -> PathBuf {
        self.0.dir.join("log.jsonl")
    }

    /// 로그를 읽는다. **깨진 줄은 건너뛴다** — 한 줄이 상했다고 나머지를 못 되돌리면 안 된다.
    fn read_log(&self) -> Vec<Record> {
        let Ok(text) = std::fs::read_to_string(self.log_path()) else {
            return Vec::new();
        };
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    fn write_log(&self, log: &[Record]) -> std::io::Result<()> {
        let mut text = String::new();
        for record in log {
            text.push_str(&serde_json::to_string(record).unwrap_or_default());
            text.push('\n');
        }
        std::fs::create_dir_all(&self.0.dir)?;
        std::fs::write(self.log_path(), text)
    }

    fn next_number(&self, log: &[Record]) -> u64 {
        log.last()
            .and_then(|r| r.backup.split('-').next()?.parse::<u64>().ok())
            .map(|n| n + 1)
            .unwrap_or(1)
    }

    /// 상한을 넘으면 오래된 것부터 버린다. 백업 파일도 같이 지운다 —
    /// 안 지우면 캐시 디렉터리가 계속 자란다.
    fn trim(&self, log: &mut Vec<Record>) {
        while log.len() > KEEP {
            let dropped = log.remove(0);
            let _ = std::fs::remove_file(self.0.dir.join(&dropped.backup));
        }
    }
}

/// 기록이 사는 곳. `XDG_CACHE_HOME`을 존중한다 — 테스트도 이것으로 자리를 옮긴다.
fn home() -> PathBuf {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("zyris-code");
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".cache/zyris-code"),
        None => std::env::temp_dir().join("zyris-code"),
    }
}

/// 작업 디렉터리를 디렉터리 이름 하나로. **해시가 아니라 읽을 수 있는 이름이다** —
/// 사람이 캐시를 들여다봤을 때 어느 리포의 것인지 알아야 지울지 말지 정할 수 있다.
fn slug(cwd: &Path) -> String {
    let mut out = String::new();
    for ch in cwd.to_string_lossy().chars() {
        match ch {
            c if c.is_alphanumeric() => out.push(c),
            _ if out.ends_with('-') => {}
            _ => out.push('-'),
        }
    }
    let name = out.trim_matches('-');
    // 아주 긴 경로는 파일시스템 이름 한도에 걸린다. 뒤쪽을 남긴다 — 리포 이름이 거기 있다.
    let cut = name.char_indices().rev().nth(NAME_LIMIT - 1).map(|(i, _)| i).unwrap_or(0);
    let name = &name[cut..];
    if name.is_empty() {
        "root".into()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 캐시 자리를 임시 디렉터리로 옮긴다. **진짜 홈에 쓰면 안 된다.**
    ///
    /// `set_var`는 프로세스 전역이라 이 테스트들은 한 줄로 세운다.
    fn scoped() -> (tempfile::TempDir, tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        static ENV: Mutex<()> = Mutex::new(());
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let cache = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CACHE_HOME", cache.path());
        (cache, work, guard)
    }

    fn write(at: &Path, text: &str) {
        std::fs::write(at, text).unwrap();
    }

    /// 되돌리면 원래 내용이 그대로 돌아와야 한다.
    #[test]
    fn reverting_puts_the_old_content_back() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "before\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        write(&file, "after\n");

        assert_eq!(undo.revert_last().unwrap(), file);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "before\n");
    }

    /// **새로 만든 파일은 지운다.** 빈 파일로 되돌리면 있지도 않던 파일이 남는다.
    #[test]
    fn reverting_a_created_file_removes_it() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("new.rs");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file); // 아직 없다
        write(&file, "새로 만든 것\n");

        undo.revert_last().unwrap();
        assert!(!file.exists(), "만든 파일이 남았다");
    }

    /// 여러 번 누르면 계속 거슬러 올라간다.
    #[test]
    fn reverting_twice_walks_back_two_edits() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "하나\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        write(&file, "둘\n");
        undo.snapshot(&file);
        write(&file, "셋\n");

        undo.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "둘\n");
        undo.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "하나\n");
        assert!(undo.is_empty(), "다 되돌렸으면 비어야 한다");
    }

    /// **여러 번 고친 파일도 한 줄이다.** 알고 싶은 것은 "다섯 번 고쳤다"가 아니라
    /// 처음과 견줘 결국 몇 줄이 달라졌는가다.
    #[test]
    fn a_file_edited_twice_counts_from_the_oldest_backup() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "하나\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        write(&file, "하나\n둘\n");
        undo.snapshot(&file);
        write(&file, "하나\n둘\n셋\n");

        let changed = undo.changed();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].path, file);
        assert_eq!(changed[0].edits, 2);
        assert_eq!((changed[0].added, changed[0].removed), (2, 0));
        assert!(!changed[0].created);
    }

    /// 없던 파일을 만든 것은 그렇다고 말해야 한다 — 되돌리면 지워진다.
    #[test]
    fn a_created_file_is_marked_as_created() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("새것.rs");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file); // 아직 없다
        write(&file, "한 줄\n");

        let changed = undo.changed();
        assert_eq!(changed.len(), 1);
        assert!(changed[0].created);
        assert_eq!((changed[0].added, changed[0].removed), (1, 0));
    }

    /// 최근에 손댄 것이 앞이다. 방금 고친 것을 목록 바닥에서 찾게 하면 안 된다.
    #[test]
    fn the_most_recently_touched_file_comes_first() {
        let (_cache, work, _g) = scoped();
        let (a, b) = (work.path().join("a.rs"), work.path().join("b.rs"));
        write(&a, "가\n");
        write(&b, "나\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&a);
        write(&a, "가가\n");
        undo.snapshot(&b);
        write(&b, "나나\n");
        // a를 한 번 더 고치면 a가 다시 앞으로 온다.
        undo.snapshot(&a);
        write(&a, "가가가\n");

        let paths: Vec<PathBuf> = undo.changed().into_iter().map(|c| c.path).collect();
        assert_eq!(paths, vec![a, b]);
    }

    /// 지워진 파일도 견줄 수 있어야 한다. 여기서 터지면 목록 전체를 못 본다.
    #[test]
    fn a_file_deleted_afterwards_still_shows_up() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "하나\n둘\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        std::fs::remove_file(&file).unwrap();

        let changed = undo.changed();
        assert_eq!(changed.len(), 1);
        assert_eq!((changed[0].added, changed[0].removed), (0, 2));
    }

    /// 되돌릴 것이 없으면 그렇게 말한다. 조용히 성공하면 눌렀는데 안 되는 줄 안다.
    #[test]
    fn reverting_with_nothing_to_undo_says_so() {
        let (_cache, work, _g) = scoped();
        let undo = Undo::for_dir(work.path());
        assert!(undo.is_empty());
        let why = undo.revert_last().unwrap_err();
        assert!(why.contains("없습니다"), "{why}");
    }

    /// **사용자 리포에 아무것도 만들지 않는다.** `.zyris-code/`를 만들면
    /// `.gitignore`에 넣어 주지 않는 한 커밋에 섞인다.
    #[test]
    fn nothing_is_written_inside_the_working_directory() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "before\n");

        Undo::for_dir(work.path()).snapshot(&file);

        let left: Vec<String> = std::fs::read_dir(work.path())
            .unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(left, vec!["a.rs".to_string()], "작업 디렉터리에 뭔가 생겼다: {left:?}");
    }

    /// 서로 다른 작업 디렉터리의 기록이 섞이면 엉뚱한 파일이 되돌아간다.
    #[test]
    fn two_working_directories_keep_separate_histories() {
        let (_cache, work, _g) = scoped();
        let other = tempfile::tempdir().unwrap();
        let file = work.path().join("a.rs");
        write(&file, "before\n");

        Undo::for_dir(work.path()).snapshot(&file);
        assert!(Undo::for_dir(other.path()).is_empty(), "남의 기록이 보인다");
    }

    /// 이름은 사람이 읽을 수 있어야 한다 — 캐시를 열었을 때 어느 리포인지 알아야 한다.
    #[test]
    fn the_directory_name_is_readable() {
        assert_eq!(slug(Path::new("/home/ruma/zyris-code")), "home-ruma-zyris-code");
        assert_eq!(slug(Path::new("/")), "root");
    }

    /// 아주 긴 경로도 파일시스템 이름 한도에 안 걸려야 한다.
    #[test]
    fn a_very_long_path_still_makes_a_usable_name() {
        let long = format!("/{}", "가나다라마바사".repeat(60));
        let name = slug(Path::new(&long));
        assert!(name.chars().count() <= NAME_LIMIT, "{}칸", name.chars().count());
        assert!(!name.is_empty());
    }
}
