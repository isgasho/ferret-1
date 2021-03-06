use crate::{
	common::assets::{AssetHandle, AssetStorage, ImportData},
	doom::{
		image::{IAColor, Image, ImageData},
		wad::read_string,
	},
};
use anyhow::{anyhow, Context};
use arrayvec::ArrayString;
use byteorder::{ReadBytesExt, LE};
use fnv::FnvHashMap;
use nalgebra::Vector2;
use relative_path::RelativePath;
use std::io::{Cursor, Read, Seek, SeekFrom};

pub fn import_flat(
	path: &RelativePath,
	asset_storage: &mut AssetStorage,
) -> anyhow::Result<Box<dyn ImportData>> {
	let mut reader = Cursor::new(asset_storage.source().load(path)?);
	let mut pixels = [0u8; 64 * 64];
	reader.read_exact(&mut pixels)?;

	Ok(Box::new(ImageData {
		data: pixels.iter().map(|&i| IAColor { i, a: 0xFF }).collect(),
		size: [64, 64],
		offset: Vector2::zeros(),
	}))
}

pub fn import_wall(
	path: &RelativePath,
	asset_storage: &mut AssetStorage,
) -> anyhow::Result<Box<dyn ImportData>> {
	let texture1_handle = asset_storage.load::<Textures>("texture1");
	let texture2_handle = if asset_storage.source().exists(RelativePath::new("texture2")) {
		Some(asset_storage.load::<Textures>("texture2"))
	} else {
		None
	};

	let texture1 = asset_storage.get(&texture1_handle).unwrap();
	let texture2 = texture2_handle.map(|h| asset_storage.get(&h).unwrap());

	let name = path.file_stem().context("Empty file name")?;
	let texture_info = texture1
		.get(name)
		.or(texture2.and_then(|t| t.get(name)))
		.ok_or(anyhow!("Texture {} does not exist", name))?
		.clone();
	let mut data = vec![IAColor::default(); texture_info.size[0] * texture_info.size[1]];

	texture_info
		.patches
		.iter()
		.try_for_each(|patch_info| -> anyhow::Result<()> {
			let patch_handle = asset_storage.load::<ImageData>(&patch_info.name);
			let patch = asset_storage.get(&patch_handle).unwrap();

			// Blit the patch onto the main image
			let dest_start = [
				std::cmp::max(patch_info.offset[0], 0),
				std::cmp::max(patch_info.offset[1], 0),
			];
			let dest_end = [
				std::cmp::min(
					patch_info.offset[0] + patch.size[0] as isize,
					texture_info.size[0] as isize,
				),
				std::cmp::min(
					patch_info.offset[1] + patch.size[1] as isize,
					texture_info.size[1] as isize,
				),
			];

			for dest_y in dest_start[1]..dest_end[1] {
				let src_y = dest_y - patch_info.offset[1];

				let dest_y_index = dest_y * texture_info.size[0] as isize;
				let src_y_index = src_y * patch.size[0] as isize;

				for dest_x in dest_start[0]..dest_end[0] {
					let src_x = dest_x - patch_info.offset[0];

					let src_index = (src_x + src_y_index) as usize;
					let dest_index = (dest_x + dest_y_index) as usize;

					data[dest_index] = patch.data[src_index];
				}
			}

			Ok(())
		})?;

	Ok(Box::new(ImageData {
		data,
		size: texture_info.size,
		offset: Vector2::zeros(),
	}))
}

pub type PNames = Vec<ArrayString<[u8; 8]>>;

pub fn import_pnames(
	path: &RelativePath,
	asset_storage: &mut AssetStorage,
) -> anyhow::Result<Box<dyn ImportData>> {
	let mut reader = Cursor::new(asset_storage.source().load(path)?);
	let count = reader.read_u32::<LE>()? as usize;
	let mut ret = Vec::with_capacity(count);

	for _ in 0..count {
		ret.push(read_string(&mut reader)?);
	}

	Ok(Box::new(ret))
}

#[derive(Clone, Debug)]
pub struct PatchInfo {
	pub offset: Vector2<isize>,
	pub name: String,
}

#[derive(Clone, Debug)]
pub struct TextureInfo {
	pub size: [usize; 2],
	pub patches: Vec<PatchInfo>,
}

pub type Textures = FnvHashMap<String, TextureInfo>;

pub fn import_textures(
	path: &RelativePath,
	asset_storage: &mut AssetStorage,
) -> anyhow::Result<Box<dyn ImportData>> {
	let pnames_handle = asset_storage.load::<PNames>("pnames");
	let pnames = asset_storage.get(&pnames_handle).unwrap();
	let mut reader = Cursor::new(asset_storage.source().load(path)?);

	let count = reader.read_u32::<LE>()? as usize;
	let mut offsets = Vec::with_capacity(count);

	for _ in 0..count {
		offsets.push(reader.read_u32::<LE>()? as u64);
	}

	Ok(Box::new(
		offsets
			.into_iter()
			.map(|offset| {
				reader.seek(SeekFrom::Start(offset))?;

				let name = read_string(&mut reader)?;
				reader.read_u32::<LE>()?; // unused
				let size = [reader.read_u16::<LE>()?, reader.read_u16::<LE>()?];
				reader.read_u32::<LE>()?; // unused
				let patch_count = reader.read_u16::<LE>()? as usize;

				let mut patches = Vec::with_capacity(patch_count);

				for _ in 0..patch_count {
					let offset = Vector2::new(
						reader.read_i16::<LE>()? as isize,
						reader.read_i16::<LE>()? as isize,
					);
					let index = reader.read_u16::<LE>()? as usize;
					let name = format!("{}.patch", pnames[index]);
					reader.read_u32::<LE>()?; // unused
					patches.push(PatchInfo { offset, name })
				}

				Ok((
					name.as_str().to_owned(),
					TextureInfo {
						size: [size[0] as usize, size[1] as usize],
						patches,
					},
				))
			})
			.collect::<anyhow::Result<Textures>>()?,
	))
}

#[derive(Clone, Debug)]
pub enum TextureType {
	Normal(AssetHandle<Image>),
	Sky,
	None,
}

impl TextureType {
	pub fn is_sky(&self) -> bool {
		if let TextureType::Sky = *self {
			true
		} else {
			false
		}
	}
}
