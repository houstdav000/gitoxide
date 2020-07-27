use anyhow::{Context, Result};
use git_features::progress::Progress;
use git_object::{owned, HashKind};
use git_odb::{loose, pack, Write};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(PartialEq, Debug)]
pub enum SafetyCheck {
    SkipFileChecksumVerification,
    SkipFileAndObjectChecksumVerification,
    SkipFileAndObjectChecksumVerificationAndNoAbortOnDecodeError,
    All,
}

impl SafetyCheck {
    pub fn variants() -> &'static [&'static str] {
        &[
            "all",
            "skip-file-checksum",
            "skip-file-and-object-checksum",
            "skip-file-and-object-checksum-and-no-abort-on-decode",
        ]
    }
}

impl std::str::FromStr for SafetyCheck {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "skip-file-checksum" => SafetyCheck::SkipFileChecksumVerification,
            "skip-file-and-object-checksum" => SafetyCheck::SkipFileAndObjectChecksumVerification,
            "skip-file-and-object-checksum-and-no-abort-on-decode" => {
                SafetyCheck::SkipFileAndObjectChecksumVerificationAndNoAbortOnDecodeError
            }
            "all" => SafetyCheck::All,
            _ => return Err(format!("Unknown value for safety check: '{}'", s)),
        })
    }
}

impl From<SafetyCheck> for pack::index::traverse::SafetyCheck {
    fn from(v: SafetyCheck) -> Self {
        use pack::index::traverse::SafetyCheck::*;
        match v {
            SafetyCheck::All => All,
            SafetyCheck::SkipFileChecksumVerification => SkipFileChecksumVerification,
            SafetyCheck::SkipFileAndObjectChecksumVerification => SkipFileAndObjectChecksumVerification,
            SafetyCheck::SkipFileAndObjectChecksumVerificationAndNoAbortOnDecodeError => {
                SkipFileAndObjectChecksumVerificationAndNoAbortOnDecodeError
            }
        }
    }
}

use quick_error::quick_error;

quick_error! {
    #[derive(Debug)]
    enum Error {
        Io(err: std::io::Error) {
            source(err)
            from()
        }
        Odb(err: loose::db::write::Error) {
            source(err)
            from()
        }
        Write(err: Box<dyn std::error::Error + Send + Sync>, kind: git_object::Kind, id: owned::Id) {
            display("Failed to write {} object {}", kind, id)
            source(&**err)
        }
        ObjectEncodeMismatch(kind: git_object::Kind, actual: owned::Id, expected: owned::Id) {
            display("{} object {} wasn't re-encoded without change - new hash is {}", kind, expected, actual)
        }
        RemoveFile(err: io::Error, index: PathBuf, data: PathBuf) {
            display("Failed to delete pack index file at '{} or data file at '{}'", index.display(), data.display())
            source(err)
        }
    }
}

struct OutputWriter(Option<loose::Db>);

impl git_odb::Write for OutputWriter {
    type Error = Error;

    fn write_stream(
        &self,
        kind: git_object::Kind,
        size: u64,
        from: impl Read,
        hash: HashKind,
    ) -> Result<owned::Id, Self::Error> {
        match self.0.as_ref() {
            Some(db) => db.write_stream(kind, size, from, hash).map_err(Into::into),
            None => git_odb::sink().write_stream(kind, size, from, hash).map_err(Into::into),
        }
    }
    fn write_buf(&self, kind: git_object::Kind, from: &[u8], hash: HashKind) -> Result<owned::Id, Self::Error> {
        match self.0.as_ref() {
            Some(db) => db.write_buf(kind, from, hash).map_err(Into::into),
            None => git_odb::sink().write_buf(kind, from, hash).map_err(Into::into),
        }
    }
}

pub fn pack_or_pack_index<P>(
    pack_path: impl AsRef<Path>,
    object_path: Option<impl AsRef<Path>>,
    check: SafetyCheck,
    thread_limit: Option<usize>,
    progress: Option<P>,
    _delete_pack: bool,
) -> Result<()>
where
    P: Progress,
    <P as Progress>::SubProgress: Send,
{
    let path = pack_path.as_ref();
    let bundle = pack::Bundle::at(path).with_context(|| {
        format!(
            "Could not find .idx or .pack file from given file at '{}'",
            path.display()
        )
    })?;

    let out = OutputWriter(object_path.map(|path| loose::Db::at(path.as_ref())));
    bundle.index.traverse(
        &bundle.pack,
        pack::index::traverse::Context {
            algorithm: pack::index::traverse::Algorithm::Lookup,
            thread_limit,
            check: check.into(),
        },
        progress,
        || {
            |object_kind, buf, index_entry, _entry_stats, progress| {
                let written_id = out
                    .write_buf(object_kind, buf, HashKind::Sha1)
                    .map_err(|err| Error::Write(Box::new(err) as Box<dyn std::error::Error + Send + Sync>, object_kind, index_entry.oid))
                    .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)?;
                if written_id != index_entry.oid {
                   if let git_object::Kind::Tree = object_kind {
                       progress.info(format!("The tree in pack named {} was written as {} due to modes 100664 and 100640 rewritten as 100644.", index_entry.oid, written_id));
                   } else {
                       return Err(Box::new(Error::ObjectEncodeMismatch(object_kind, index_entry.oid, written_id)))
                   }
                }
                Ok(())
            }
        },
        pack::cache::DecodeEntryLRU::default,
    ).with_context(|| "Some loose objects could not be extracted")?;

    let (index_path, data_path) = (bundle.index.path().to_owned(), bundle.pack.path().to_owned());
    drop(bundle);

    fs::remove_file(&index_path)
        .and_then(|_| fs::remove_file(&data_path))
        .map_err(|err| Error::RemoveFile(err, index_path, data_path))
        .map_err(Into::into)
}