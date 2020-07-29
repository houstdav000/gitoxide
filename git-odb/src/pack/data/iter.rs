use crate::pack;
use quick_error::quick_error;
use std::{fs, io, io::Seek};

#[derive(Debug)]
pub struct Iter<'a, R> {
    read: R,
    compressed_bytes: Vec<u8>,
    decompressed_bytes: Vec<u8>,
    offset: u64,
    _item_lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a, R> Iter<'a, R>
where
    R: io::Read,
{
    fn buffers() -> (Vec<u8>, Vec<u8>) {
        let base = 4096;
        (Vec::with_capacity(base * 2), Vec::with_capacity(base * 4))
    }
    // Note that `read` is expected to start right past the header
    pub fn new_from_header(
        mut read: R,
    ) -> io::Result<
        Result<(pack::data::Kind, u32, impl Iterator<Item = Result<Entry<'a>, Error>>), pack::data::parse::Error>,
    > {
        let mut header_data = [0u8; 12];
        read.read_exact(&mut header_data)?;

        Ok(pack::data::parse::header(&header_data).map(|(kind, num_objects)| {
            (
                kind,
                num_objects,
                Iter::new_from_first_entry(read, 12).take(num_objects as usize),
            )
        }))
    }

    /// `read` must be placed right past the header, and this iterator will fail ungracefully once
    /// it goes past the last object in the pack, i.e. will choke on the trailer if present.
    /// Hence you should only use it with `take(num_objects)`.
    /// Alternatively, use `new_from_header()`
    ///
    /// `offset` is the amount of bytes consumed from `read`, usually the size of the header, for use as offset into the pack.
    /// when resolving ref deltas to their absolute pack offset.
    pub fn new_from_first_entry(read: R, offset: u64) -> Self {
        let (compressed_bytes, decompressed_bytes) = Self::buffers();
        Iter {
            read,
            compressed_bytes,
            decompressed_bytes,
            offset,
            _item_lifetime: std::marker::PhantomData,
        }
    }

    fn next_inner(&mut self) -> Result<Entry<'a>, Error> {
        pack::data::Header::from_read(&mut self.read, self.offset).map_err(Error::from)?;
        unimplemented!("inner next");
    }
}

quick_error! {
    #[derive(Debug)]
    pub enum Error {
        Io(err: io::Error) {
            display("An IO operation failed while streaming an entry")
            from()
            source(err)
        }
        Zlib(err: crate::zlib::Error) {
            display("The stream could not be decompressed")
            source(err)
            from()
        }
    }
}

#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub struct Entry<'a> {
    pub header: pack::data::Header,
    pub pack_offset: u64,
    /// The compressed data making up this entry
    #[cfg_attr(feature = "serde1", serde(borrow))]
    pub compressed: &'a [u8],
    /// The decompressed data (stemming from `compressed`)
    pub decompressed: &'a [u8],
}

impl<'a, R> Iterator for Iter<'a, R>
where
    R: io::Read,
{
    type Item = Result<Entry<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.next_inner())
    }
}

impl pack::data::File {
    /// Note that this iterator is costly as no pack index is used, forcing each entry to be decompressed.
    /// If an index is available, use the `traverse(…)` method instead for maximum performance.
    pub fn iter(&self) -> io::Result<(pack::data::Kind, u32, impl Iterator<Item = Result<Entry<'_>, Error>>)> {
        let mut reader = io::BufReader::new(fs::File::open(&self.path)?);
        reader.seek(io::SeekFrom::Current(12))?;
        Ok((self.kind, self.num_objects, Iter::new_from_first_entry(reader, 12)))
    }
}