use crate::{DocumentError, ResourceLimits};
use std::io::{Cursor, Read};

pub(crate) struct Package<'a> {
    archive: zip::ZipArchive<Cursor<&'a [u8]>>,
    max_entry_bytes: u64,
}

impl<'a> Package<'a> {
    pub(crate) fn open(bytes: &'a [u8], limits: &ResourceLimits) -> Result<Self, DocumentError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| DocumentError::malformed(error.to_string()))?;
        if archive.len() > limits.max_archive_entries {
            return Err(DocumentError::ResourceLimit {
                limit: "max_archive_entries",
                detail: format!("archive contains {} entries", archive.len()),
            });
        }
        let mut total = 0u64;
        for index in 0..archive.len() {
            let entry = archive
                .by_index_raw(index)
                .map_err(|error| DocumentError::malformed(error.to_string()))?;
            total = total.saturating_add(entry.size());
        }
        if total > limits.max_total_uncompressed_bytes {
            return Err(DocumentError::ResourceLimit {
                limit: "max_total_uncompressed_bytes",
                detail: format!("archive declares {total} uncompressed bytes"),
            });
        }
        Ok(Self {
            archive,
            max_entry_bytes: limits.max_entry_bytes,
        })
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.archive.file_names()
    }

    pub(crate) fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, DocumentError> {
        let Ok(mut entry) = self.archive.by_name(name) else {
            return Ok(None);
        };
        if entry.size() > self.max_entry_bytes {
            return Err(DocumentError::ResourceLimit {
                limit: "max_entry_bytes",
                detail: format!("package part {name:?} is {} bytes", entry.size()),
            });
        }
        let capacity = usize::try_from(entry.size()).unwrap_or(0);
        let mut bytes = Vec::with_capacity(capacity);
        entry.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    pub(crate) fn read_required(&mut self, name: &str) -> Result<Vec<u8>, DocumentError> {
        self.read(name)?.ok_or_else(|| DocumentError::MissingPart {
            part: name.to_owned(),
        })
    }
}
