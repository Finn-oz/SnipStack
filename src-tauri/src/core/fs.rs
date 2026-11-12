//! 文件落盘小工具。

use std::path::Path;

/// 内容寻址文件的幂等写入：目标已存在则跳过，否则先写同目录临时文件再 rename 到位。
///
/// 调用方都拿「文件存在」当「内容已写完」。直接 `fs::write` 到最终路径时，磁盘写满、
/// 进程被杀或断电会留下截断文件，之后每次同内容写入都因「已存在」跳过，坏文件被
/// 永久复用；并发读者也可能读到写了一半的内容。rename 保证最终路径上要么没有文件，
/// 要么是完整内容。
pub fn write_if_absent(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // 临时名带随机后缀：同一内容被两个线程同时写入时互不踩对方的临时文件。
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let tmp = path.with_file_name(tmp_name);

    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    let Err(err) = written else {
        return Ok(());
    };

    let _ = std::fs::remove_file(&tmp);
    // 并发写同一内容时对方先 rename 到位（Windows 上目标正被读取时 rename 会被拒）：
    // 最终文件已完整存在，按成功处理。
    if path.exists() {
        return Ok(());
    }
    Err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_new_file_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("a.png");

        write_if_absent(&target, b"hello").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        let names: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.png"]);
    }

    #[test]
    fn keeps_existing_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.png");
        std::fs::write(&target, b"first").unwrap();

        write_if_absent(&target, b"second").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"first");
    }
}
