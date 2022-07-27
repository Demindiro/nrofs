use fuser::*;
use std::{
	collections::BTreeMap,
	ffi::OsStr,
	fs::File,
	io::{Read, Seek},
	os::unix::ffi::OsStrExt,
	time::{Duration, UNIX_EPOCH},
};

const TTL: Duration = Duration::MAX;

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let mut a = std::env::args().skip(1);
	let f = a.next().ok_or("expected file path")?;
	let m = a.next().ok_or("expected mount path")?;

	fuser::mount2(
		Fs::new(File::open(&f)?),
		m,
		&[MountOption::RO, MountOption::FSName("nrofs".into())],
	)?;
	Ok(())
}

struct Fs {
	io: std::cell::RefCell<File>,
	header: nrofs::Header,
	dirs: Vec<BTreeMap<Box<[u8]>, Node>>,
}

impl Fs {
	fn new(mut io: File) -> Self {
		let mut s = Self {
			header: nrofs::Header::load(|b| io.read_exact(b)).unwrap(),
			io: io.into(),
			dirs: Default::default(),
		};
		let mut dirs = vec![BTreeMap::default()];
		let mut buf = [0; 255];
		for (i, e) in s
			.header
			.iter(|o| s.do_io(o))
			.unwrap()
			.map(Result::unwrap)
			.enumerate()
		{
			let name = e.name(&mut buf, |o| s.do_io(o)).unwrap();
			let mut di = 0;
			let mut it = name
				.split(|&c| c == b'/')
				.filter(|p| !p.is_empty())
				.peekable();
			while let Some(p) = it.next() {
				let l = dirs.len().try_into().unwrap();
				let d = dirs[di as usize].entry(p.into());
				if it.peek().is_some() {
					// dir
					di = match d.or_insert(Node::File(u32::MAX)) {
						Node::Dir(n) => *n,
						d @ Node::File(_) => {
							*d = Node::Dir(l);
							dirs.push(Default::default());
							l
						}
					};
				} else {
					// file
					d.or_insert_with(|| Node::File(i.try_into().unwrap()));
				}
			}
		}
		s.dirs = dirs;
		s
	}

	fn attr(&self, ty: FileType, size: u64, ino: u64) -> FileAttr {
		let block_mask = (1u64 << self.header.block_size()) - 1;
		FileAttr {
			atime: UNIX_EPOCH,
			mtime: UNIX_EPOCH,
			ctime: UNIX_EPOCH,
			crtime: UNIX_EPOCH,
			perm: 0o777,
			nlink: 1,
			uid: 0,
			gid: 0,
			rdev: 0,
			flags: 0,
			kind: ty,
			size,
			blocks: (size + block_mask) & !block_mask,
			ino,
			blksize: 1 << self.header.block_size(),
		}
	}

	fn entry(&self, ino: u64) -> Option<Node> {
		let n = ino - 1;
		if n & 1 << 63 != 0 {
			(n ^ (1 << 63) < self.header.file_count().into()).then(|| Node::File(n as _))
		} else {
			(n < self.dirs.len() as u64).then(|| Node::Dir(n as _))
		}
	}

	fn do_io(&self, op: nrofs::Op) -> Result<(), std::io::Error> {
		let mut io = self.io.borrow_mut();
		match op {
			nrofs::Op::Seek(p) => io.seek(std::io::SeekFrom::Start(p)).map(|_| ()),
			nrofs::Op::Advance(p) => io.seek(std::io::SeekFrom::Current(p)).map(|_| ()),
			nrofs::Op::Read(b) => io.read_exact(b),
		}
	}
}

#[derive(Clone, Copy, Debug)]
enum Node {
	File(u32),
	Dir(u32),
}

impl Node {
	fn to_ino(&self) -> u64 {
		1 + match *self {
			Self::File(n) => u64::from(n) | 1 << 63,
			Self::Dir(n) => n.into(),
		}
	}

	fn to_ty(&self) -> FileType {
		match self {
			Self::File(_) => FileType::RegularFile,
			Self::Dir(_) => FileType::Directory,
		}
	}

	fn size(&self, fs: &Fs) -> u64 {
		match *self {
			Self::File(n) => fs.header.get(n, |o| fs.do_io(o)).unwrap().unwrap().size() as _,
			Self::Dir(n) => fs.dirs[n as usize].len() as _,
		}
	}
}

impl Filesystem for Fs {
	fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
		match self.entry(parent) {
			Some(Node::Dir(n)) => {
				if let Some(e) = self.dirs[n as usize].get(name.as_bytes()) {
					reply.entry(&TTL, &self.attr(e.to_ty(), e.size(self), e.to_ino()), 0)
				} else {
					reply.error(libc::ENOENT)
				}
			}
			_ => reply.error(libc::ENOENT),
		}
	}

	fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
		match self.entry(ino) {
			Some(e) => reply.attr(&TTL, &self.attr(e.to_ty(), e.size(self), e.to_ino())),
			None => reply.error(libc::ENOENT),
		}
	}

	fn read(
		&mut self,
		_req: &Request,
		ino: u64,
		_fh: u64,
		offset: i64,
		size: u32,
		_flags: i32,
		_lock: Option<u64>,
		reply: ReplyData,
	) {
		let mut buf = [0; 1 << 16];
		match self.entry(ino) {
			Some(Node::File(n)) => {
				let e = self.header.get(n, |o| self.do_io(o)).unwrap().unwrap();
				let offt = i128::from(e.offset(&self.header)) + i128::from(offset);
				let size = (e.size() as i128 - i128::from(offset))
					.min(size as _)
					.min(buf.len() as _)
					.max(0);
				self.io
					.borrow_mut()
					.seek(std::io::SeekFrom::Start(offt.try_into().unwrap()))
					.unwrap();
				self.io.borrow_mut().read(&mut buf[..size as _]).unwrap();
				reply.data(&buf[..size as _]);
			}
			_ => reply.error(libc::ENOENT),
		}
	}

	fn readdir(
		&mut self,
		_req: &Request<'_>,
		ino: u64,
		_fh: u64,
		mut offset: i64,
		mut reply: ReplyDirectory,
	) {
		let n = if let Some(Node::Dir(n)) = self.entry(ino) {
			n
		} else {
			reply.error(libc::ENOENT);
			return;
		};

		if offset == 0 {
			if reply.add(Node::Dir(n).to_ino(), 1, FileType::Directory, ".") {
				return reply.ok();
			}
			offset += 1;
		}

		if offset == 1 {
			if reply.add(Node::Dir(n).to_ino(), 2, FileType::Directory, "..") {
				return reply.ok();
			}
			offset += 1;
		}

		for (i, (k, v)) in self.dirs[n as usize]
			.iter()
			.enumerate()
			.skip((offset - 2) as _)
		{
			if reply.add((i + 2) as _, (i + 3) as _, v.to_ty(), OsStr::from_bytes(k)) {
				break;
			}
		}

		reply.ok();
	}
}
