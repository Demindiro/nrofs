#![no_std]

const MAGIC: [u8; 8] = *b"NrRdOnly";
const VERSION: u8 = 0;

fn parse_header<R>(d: [u8; 16]) -> Result<(u8, u32), ParseHeaderError<R>> {
	if &d[..8] != &MAGIC {
		Err(ParseHeaderError::BadMagic)
	} else if d[8] != VERSION {
		Err(ParseHeaderError::UnsupportedVersion)
	} else {
		Ok((d[9], u32::from_le_bytes(d[12..].try_into().unwrap())))
	}
}

fn parse_entry(d: [u8; 12]) -> Entry {
	let f = |d: &[_]| u32::from_le_bytes(d.try_into().unwrap());
	let (filename_addr, block_addr, file_size) = (f(&d[..4]), f(&d[4..8]), f(&d[8..]));
	Entry { filename_addr, block_addr, file_size }
}

fn read_string<R>(
	buf: &mut [u8; 255],
	mut f: impl FnMut(&mut [u8]) -> Result<(), R>,
) -> Result<&[u8], R> {
	let mut l = [0];
	f(&mut l)?;
	let buf = &mut buf[..l[0].into()];
	f(buf).map(|()| &*buf)
}

#[derive(Debug)]
pub enum ParseHeaderError<R> {
	BadMagic,
	UnsupportedVersion,
	Other(R),
}

#[derive(Debug)]
pub struct Header {
	block_size: u8,
	file_count: u32,
}

#[derive(Debug)]
pub struct Entry {
	filename_addr: u32,
	block_addr: u32,
	file_size: u32,
}

pub enum Op<'a> {
	Seek(u64),
	Advance(i64),
	Read(&'a mut [u8]),
}

impl Header {
	pub fn load<R, Io>(mut io: Io) -> Result<Self, ParseHeaderError<R>>
	where
		Io: FnMut(&mut [u8]) -> Result<(), R>,
	{
		let mut b = [0; 16];
		io(&mut b)
			.map_err(ParseHeaderError::Other)
			.and_then(|()| parse_header(b))
			.map(|(block_size, file_count)| Self { block_size, file_count })
	}

	pub fn get<R, Io>(&self, index: u32, io: Io) -> Option<Result<Entry, R>>
	where
		Io: FnMut(Op<'_>) -> Result<(), R>,
	{
		(index < self.file_count).then(|| get(index, io))
	}

	pub fn iter<R, Io>(&self, mut io: Io) -> Result<Iter<R, Io>, R>
	where
		Io: FnMut(Op<'_>) -> Result<(), R>,
	{
		io(Op::Seek(16)).map(|()| Iter { io, count: self.file_count, offset: 0 })
	}

	pub fn file_count(&self) -> u32 {
		self.file_count
	}

	pub fn block_size(&self) -> u8 {
		self.block_size
	}
}

impl Entry {
	pub fn name<'a, R, Io>(&self, buf: &'a mut [u8; 255], mut io: Io) -> Result<&'a [u8], R>
	where
		Io: FnMut(Op<'_>) -> Result<(), R>,
	{
		io(Op::Seek(self.filename_addr.into()))?;
		read_string(buf, |b| io(Op::Read(b)))
	}

	pub fn block(&self) -> u32 {
		self.block_addr
	}

	pub fn offset(&self, header: &Header) -> u64 {
		u64::from(self.block_addr) << header.block_size
	}

	pub fn size(&self) -> u32 {
		self.file_size
	}
}

pub struct Iter<R, Io>
where
	Io: FnMut(Op<'_>) -> Result<(), R>,
{
	io: Io,
	offset: u32,
	count: u32,
}

impl<R, Io> Iterator for Iter<R, Io>
where
	Io: FnMut(Op<'_>) -> Result<(), R>,
{
	type Item = Result<Entry, R>;

	fn next(&mut self) -> Option<Self::Item> {
		(self.offset < self.count).then(|| {
			let o = self.offset;
			self.offset += 1;
			get(o, &mut self.io)
		})
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		(self.len(), Some(self.len()))
	}

	fn count(self) -> usize {
		self.len()
	}

	fn nth(&mut self, n: usize) -> Option<Self::Item> {
		let n = u32::try_from(n).unwrap_or(u32::MAX);
		self.offset = self.offset.saturating_add(n);
		self.next()
	}
}

impl<R, Io> ExactSizeIterator for Iter<R, Io>
where
	Io: FnMut(Op<'_>) -> Result<(), R>,
{
	fn len(&self) -> usize {
		(self.count - self.offset).try_into().unwrap()
	}
}

fn get<R, Io>(index: u32, mut io: Io) -> Result<Entry, R>
where
	Io: FnMut(Op<'_>) -> Result<(), R>,
{
	io(Op::Seek(16 + u64::from(index) * 12))?;
	let mut b = [0; 12];
	io(Op::Read(&mut b))?;
	Ok(parse_entry(b))
}
