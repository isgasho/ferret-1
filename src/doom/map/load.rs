use crate::{
	assets::{Asset, AssetFormat, AssetStorage, DataSource},
	doom::{
		map::{
			textures::{Flat, TextureType, WallTexture},
			GLNode, GLSSect, GLSeg, Linedef, Map, NodeChild, Sector, Sidedef,
		},
		physics::SolidMask,
		wad::WadLoader,
	},
	geometry::{Angle, Interval, Line2, Plane, Side, AABB2},
};
use anyhow::{bail, ensure};
use bitflags::bitflags;
use byteorder::{ReadBytesExt, LE};
use nalgebra::{Vector2, Vector3};
use serde::Deserialize;
use std::{
	collections::hash_map::{Entry, HashMap},
	io::{Cursor, Read},
};

pub struct MapData {
	pub linedefs: Vec<LinedefData>,
	pub sidedefs: Vec<SidedefData>,
	pub vertexes: Vec<Vector2<f32>>,
	pub sectors: Vec<SectorData>,
	pub gl_vert: Vec<Vector2<f32>>,
	pub gl_segs: Vec<GLSegData>,
	pub gl_ssect: Vec<GLSSectData>,
	pub gl_nodes: Vec<GLNodeData>,
}

impl Asset for Map {
	type Data = Self;
	type Intermediate = MapData;
	const NAME: &'static str = "Map";

	fn import(name: &str, source: &impl DataSource) -> anyhow::Result<Self::Intermediate> {
		let gl_name = format!("GL_{}", name);

		let linedefs = LinedefsFormat.import(name, source)?;
		let sidedefs = SidedefsFormat.import(name, source)?;
		let vertexes = VertexesFormat.import(name, source)?;
		let sectors = SectorsFormat.import(name, source)?;

		let gl_vert = GLVertFormat.import(&gl_name, source)?;
		let gl_segs = GLSegsFormat.import(&gl_name, source)?;
		let gl_ssect = GLSSectFormat.import(&gl_name, source)?;
		let gl_nodes = GLNodesFormat.import(&gl_name, source)?;

		// Verify all the cross-references

		for (i, sidedef) in sidedefs.iter().enumerate() {
			ensure!(
				sidedef.sector_index < sectors.len(),
				"Sidedef {} has invalid sector index {}",
				i,
				sidedef.sector_index
			);
		}

		for (i, linedef) in linedefs.iter().enumerate() {
			for index in linedef.sidedef_indices.iter().flatten() {
				ensure!(
					*index < sidedefs.len(),
					"Linedef {} has invalid sidedef index {}",
					i,
					index
				);
			}
		}

		for (i, seg) in gl_segs.iter().enumerate() {
			if let Some(index) = seg.linedef_index {
				ensure!(
					index < linedefs.len(),
					"Seg {} has invalid linedef index {}",
					i,
					index
				);
			}

			for index in seg.vertex_indices.iter() {
				match *index {
					EitherVertex::GL(index) => ensure!(
						index < gl_vert.len(),
						"Seg {} has invalid vertex index {}",
						i,
						index
					),
					EitherVertex::Normal(index) => ensure!(
						index < vertexes.len(),
						"Seg {} has invalid vertex index {}",
						i,
						index
					),
				}
			}

			if let Some(index) = seg.partner_seg_index {
				ensure!(
					index < gl_segs.len(),
					"Seg {} has invalid partner seg index {}",
					i,
					index
				);
			}
		}

		for (i, ssect) in gl_ssect.iter().enumerate() {
			ensure!(
				ssect.first_seg_index < gl_segs.len(),
				"Subsector {} has invalid first seg index {}",
				i,
				ssect.first_seg_index
			);
			ensure!(
				ssect.first_seg_index + ssect.seg_count <= gl_segs.len(),
				"Subsector {} has overflowing seg count {}",
				i,
				ssect.seg_count
			);
		}

		for (i, node) in gl_nodes.iter().enumerate() {
			for child in node.child_indices.iter().copied() {
				match child {
					NodeChild::Subsector(index) => ensure!(
						(index as usize) < gl_ssect.len(),
						"Node {} has invalid subsector index {}",
						i,
						index
					),
					NodeChild::Node(index) => ensure!(
						(index as usize) < gl_nodes.len(),
						"Node {} has invalid child node index {}",
						i,
						index
					),
				}
			}
		}

		Ok(MapData {
			linedefs,
			sidedefs,
			vertexes,
			sectors,
			gl_vert,
			gl_segs,
			gl_ssect,
			gl_nodes,
		})
	}
}

pub fn build_map(
	map_data: MapData,
	sky_name: &str,
	loader: &mut WadLoader,
	flat_storage: &mut AssetStorage<Flat>,
	wall_texture_storage: &mut AssetStorage<WallTexture>,
) -> anyhow::Result<Map> {
	let mut textures = HashMap::new();
	let mut flats = HashMap::new();
	let sky = wall_texture_storage.load(sky_name, loader);

	let MapData {
		linedefs: linedefs_data,
		sidedefs: sidedefs_data,
		vertexes: vertexes_data,
		sectors: sectors_data,
		gl_vert: gl_vert_data,
		gl_segs: gl_segs_data,
		gl_ssect: gl_ssect_data,
		gl_nodes: gl_nodes_data,
	} = map_data;

	let mut sectors = sectors_data
		.into_iter()
		.map(|data| {
			Ok(Sector {
				interval: Interval::new(data.floor_height, data.ceiling_height),
				floor_texture: match data.floor_flat_name {
					None => TextureType::None,
					Some(name) if name == "F_SKY1" => TextureType::Sky,
					Some(name) => {
						let handle = match flats.entry(name) {
							Entry::Vacant(entry) => {
								let handle = flat_storage.load(entry.key(), &mut *loader);
								entry.insert(handle)
							}
							Entry::Occupied(entry) => entry.into_mut(),
						};
						TextureType::Normal(handle.clone())
					}
				},
				ceiling_texture: match data.ceiling_flat_name {
					None => TextureType::None,
					Some(name) if name == "F_SKY1" => TextureType::Sky,
					Some(name) => {
						let handle = match flats.entry(name) {
							Entry::Vacant(entry) => {
								let handle = flat_storage.load(entry.key(), &mut *loader);
								entry.insert(handle)
							}
							Entry::Occupied(entry) => entry.into_mut(),
						};
						TextureType::Normal(handle.clone())
					}
				},
				light_level: data.light_level,
				special_type: data.special_type,
				sector_tag: data.special_type,
				subsectors: Vec::new(),
				neighbours: Vec::new(),
			})
		})
		.collect::<anyhow::Result<Vec<Sector>>>()?;

	let mut sidedefs = sidedefs_data
		.into_iter()
		.map(|data| {
			Ok(Some(Sidedef {
				texture_offset: data.texture_offset,
				top_texture: match data.top_texture_name {
					None => TextureType::None,
					Some(name) if name == "F_SKY1" => TextureType::Sky,
					Some(name) => {
						let handle = match textures.entry(name) {
							Entry::Vacant(entry) => {
								let handle = wall_texture_storage.load(entry.key(), &mut *loader);
								entry.insert(handle)
							}
							Entry::Occupied(entry) => entry.into_mut(),
						};
						TextureType::Normal(handle.clone())
					}
				},
				bottom_texture: match data.bottom_texture_name {
					None => TextureType::None,
					Some(name) => {
						let handle = match textures.entry(name) {
							Entry::Vacant(entry) => {
								let handle = wall_texture_storage.load(entry.key(), &mut *loader);
								entry.insert(handle)
							}
							Entry::Occupied(entry) => entry.into_mut(),
						};
						TextureType::Normal(handle.clone())
					}
				},
				middle_texture: match data.middle_texture_name {
					None => TextureType::None,
					Some(name) => {
						let handle = match textures.entry(name) {
							Entry::Vacant(entry) => {
								let handle = wall_texture_storage.load(entry.key(), &mut *loader);
								entry.insert(handle)
							}
							Entry::Occupied(entry) => entry.into_mut(),
						};
						TextureType::Normal(handle.clone())
					}
				},
				sector_index: data.sector_index,
			}))
		})
		.collect::<anyhow::Result<Vec<Option<Sidedef>>>>()?;

	let linedefs = linedefs_data
		.into_iter()
		.map(|data| {
			let mut sidedefs = [
				data.sidedef_indices[0].map(|x| sidedefs[x].take().unwrap()),
				data.sidedef_indices[1].map(|x| sidedefs[x].take().unwrap()),
			];

			if let [Some(ref mut front_sidedef), Some(ref mut back_sidedef)] = &mut sidedefs {
				// Set sector neighbours
				if front_sidedef.sector_index != back_sidedef.sector_index {
					let front_sector_neighbours =
						&mut sectors[front_sidedef.sector_index].neighbours;
					if !front_sector_neighbours.contains(&back_sidedef.sector_index) {
						front_sector_neighbours.push(back_sidedef.sector_index);
					}

					let back_sector_neighbours = &mut sectors[back_sidedef.sector_index].neighbours;
					if !back_sector_neighbours.contains(&front_sidedef.sector_index) {
						back_sector_neighbours.push(front_sidedef.sector_index);
					}
				}

				// If an upper texture is neighboured by two sky flats, make it sky too
				if sectors[front_sidedef.sector_index].ceiling_texture.is_sky()
					&& sectors[back_sidedef.sector_index].ceiling_texture.is_sky()
				{
					front_sidedef.top_texture = TextureType::Sky;
					back_sidedef.top_texture = TextureType::Sky;
				}
			}

			let dir = vertexes_data[data.vertex_indices[1]] - vertexes_data[data.vertex_indices[0]];
			let line = Line2::new(vertexes_data[data.vertex_indices[0]], dir);
			let normal = Vector2::new(dir[1], -dir[0]).normalize();
			let bbox = {
				let mut bbox = AABB2::empty();
				bbox.add_point(vertexes_data[data.vertex_indices[0]]);
				bbox.add_point(vertexes_data[data.vertex_indices[1]]);
				bbox
			};

			let mut planes = Vec::from(&bbox.planes()[..]);

			if normal[0] != 0.0 && normal[1] != 0.0 {
				planes.push(Plane {
					distance: line.point.dot(&normal),
					normal: Vector3::new(normal[0], normal[1], 0.0),
				});
				planes.push(Plane {
					distance: -line.point.dot(&normal),
					normal: Vector3::new(-normal[0], -normal[1], 0.0),
				});
			}

			Ok(Linedef {
				line,
				normal,
				planes,
				bbox,
				flags: data.flags,
				solid_mask: if data.flags.intersects(LinedefFlags::BLOCKING) {
					SolidMask::all()
				} else if data.flags.intersects(LinedefFlags::BLOCKMONSTERS) {
					SolidMask::MONSTER
				} else {
					SolidMask::empty()
				},
				special_type: data.special_type,
				sector_tag: data.sector_tag,
				sidedefs,
			})
		})
		.collect::<anyhow::Result<Vec<Linedef>>>()?;

	let nodes_len = gl_nodes_data.len();
	let nodes = gl_nodes_data
		.into_iter()
		.rev()
		.map(|data| {
			Ok(GLNode {
				partition_line: Line2::new(data.partition_point, data.partition_dir),
				normal: Vector2::new(data.partition_dir[1], -data.partition_dir[0]).normalize(),
				child_bboxes: data.child_bboxes.clone(),
				child_indices: [
					match data.child_indices[0] {
						NodeChild::Subsector(index) => NodeChild::Subsector(index),
						NodeChild::Node(index) => NodeChild::Node(nodes_len - index - 1),
					},
					match data.child_indices[1] {
						NodeChild::Subsector(index) => NodeChild::Subsector(index),
						NodeChild::Node(index) => NodeChild::Node(nodes_len - index - 1),
					},
				],
			})
		})
		.collect::<anyhow::Result<Vec<GLNode>>>()?;

	let mut segs = gl_segs_data
		.into_iter()
		.map(|data| {
			let vertices = [
				match data.vertex_indices[0] {
					EitherVertex::GL(index) => gl_vert_data[index],
					EitherVertex::Normal(index) => vertexes_data[index],
				},
				match data.vertex_indices[1] {
					EitherVertex::GL(index) => gl_vert_data[index],
					EitherVertex::Normal(index) => vertexes_data[index],
				},
			];
			let dir = vertices[1] - vertices[0];

			Ok(GLSeg {
				line: Line2::new(vertices[0], dir),
				normal: Vector2::new(dir[1], -dir[0]).normalize(),
				linedef_index: data.linedef_index,
				linedef_side: data.linedef_side,
				//partner_seg_index: data.partner_seg_index,
			})
		})
		.collect::<anyhow::Result<Vec<GLSeg>>>()?;

	let subsectors = gl_ssect_data
		.into_iter()
		.enumerate()
		.map(|(i, ssect)| {
			let segs = &mut segs[ssect.first_seg_index as usize
				..ssect.first_seg_index as usize + ssect.seg_count as usize];

			let sector_index = {
				if let Some(sidedef) = segs.iter().find_map(|seg| match seg.linedef_index {
					None => None,
					Some(index) => linedefs[index].sidedefs[seg.linedef_side as usize].as_ref(),
				}) {
					sidedef.sector_index
				} else {
					bail!("No sector could be found for subsector {}", i);
				}
			};

			let bbox = {
				let mut bbox = AABB2::empty();
				for seg in segs.iter() {
					bbox.add_point(seg.line.point);
				}
				bbox
			};

			let mut planes = Vec::from(&bbox.planes()[..]);

			planes.extend(segs.iter().filter_map(|seg| {
				if seg.normal[0] != 0.0 && seg.normal[1] != 0.0 {
					Some(Plane {
						distance: seg.line.point.dot(&-seg.normal),
						normal: Vector3::new(-seg.normal[0], -seg.normal[1], 0.0),
					})
				} else {
					None
				}
			}));

			sectors[sector_index].subsectors.push(i);

			Ok(GLSSect {
				segs: segs.to_owned(),
				planes,
				sector_index,
				bbox,
			})
		})
		.collect::<anyhow::Result<Vec<GLSSect>>>()?;

	Ok(Map {
		linedefs,
		sectors,
		subsectors,
		nodes,
		sky,
	})
}

pub struct ThingData {
	pub position: Vector2<f32>,
	pub angle: Angle,
	pub doomednum: u16,
	pub flags: ThingFlags,
}

bitflags! {
	#[derive(Deserialize)]
	pub struct ThingFlags: u16 {
		const EASY = 0b00000000_00000001;
		const NORMAL = 0b00000000_00000010;
		const HARD = 0b00000000_00000100;
		const MPONLY = 0b00000000_00001000;
	}
}

#[derive(Clone, Copy)]
pub struct ThingsFormat;

impl AssetFormat for ThingsFormat {
	type Asset = Vec<ThingData>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 1))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			ret.push(ThingData {
				position: Vector2::new(
					reader.read_i16::<LE>()? as f32,
					reader.read_i16::<LE>()? as f32,
				),
				angle: Angle::from_degrees(reader.read_u16::<LE>()? as f64),
				doomednum: reader.read_u16::<LE>()?,
				flags: ThingFlags::from_bits_truncate(reader.read_u16::<LE>()?),
			});
		}

		Ok(ret)
	}
}

pub struct LinedefData {
	pub vertex_indices: [usize; 2],
	pub flags: LinedefFlags,
	pub special_type: u16,
	pub sector_tag: u16,
	pub sidedef_indices: [Option<usize>; 2],
}

bitflags! {
	#[derive(Deserialize)]
	pub struct LinedefFlags: u16 {
		const BLOCKING = 0b00000000_00000001;
		const BLOCKMONSTERS = 0b00000000_00000010;
		const TWOSIDED = 0b00000000_00000100;
		const DONTPEGTOP = 0b00000000_00001000;
		const DONTPEGBOTTOM = 0b00000000_00010000;
		const SECRET = 0b00000000_00100000;
		const BLOCKSOUND = 0b00000000_01000000;
		const NOAUTOMAP = 0b00000000_10000000;
	}
}

#[derive(Clone, Copy)]
pub struct LinedefsFormat;

impl AssetFormat for LinedefsFormat {
	type Asset = Vec<LinedefData>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 2))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			ret.push(LinedefData {
				vertex_indices: [
					reader.read_u16::<LE>()? as usize,
					reader.read_u16::<LE>()? as usize,
				],
				flags: LinedefFlags::from_bits_truncate(reader.read_u16::<LE>()?),
				special_type: reader.read_u16::<LE>()?,
				sector_tag: reader.read_u16::<LE>()?,
				sidedef_indices: [
					match reader.read_u16::<LE>()? as usize {
						0xFFFF => None,
						x => Some(x),
					},
					match reader.read_u16::<LE>()? as usize {
						0xFFFF => None,
						x => Some(x),
					},
				],
			});
		}

		Ok(ret)
	}
}

pub struct SidedefData {
	pub texture_offset: Vector2<f32>,
	pub top_texture_name: Option<String>,
	pub bottom_texture_name: Option<String>,
	pub middle_texture_name: Option<String>,
	pub sector_index: usize,
}

#[derive(Clone, Copy)]
pub struct SidedefsFormat;

impl AssetFormat for SidedefsFormat {
	type Asset = Vec<SidedefData>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 3))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			let mut buf = [0u8; 8];

			ret.push(SidedefData {
				texture_offset: Vector2::new(
					reader.read_i16::<LE>()? as f32,
					reader.read_i16::<LE>()? as f32,
				),
				top_texture_name: match {
					reader.read_exact(&mut buf)?;
					&buf
				} {
					b"-\0\0\0\0\0\0\0" => None,
					x => Some(std::str::from_utf8(x)?.trim_end_matches('\0').to_owned()),
				},
				bottom_texture_name: match {
					reader.read_exact(&mut buf)?;
					&buf
				} {
					b"-\0\0\0\0\0\0\0" => None,
					x => Some(std::str::from_utf8(x)?.trim_end_matches('\0').to_owned()),
				},
				middle_texture_name: match {
					reader.read_exact(&mut buf)?;
					&buf
				} {
					b"-\0\0\0\0\0\0\0" => None,
					x => Some(std::str::from_utf8(x)?.trim_end_matches('\0').to_owned()),
				},
				sector_index: reader.read_u16::<LE>()? as usize,
			});
		}

		Ok(ret)
	}
}

#[derive(Clone, Copy)]
pub struct VertexesFormat;

impl AssetFormat for VertexesFormat {
	type Asset = Vec<Vector2<f32>>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 4))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			ret.push(Vector2::new(
				reader.read_i16::<LE>()? as f32,
				reader.read_i16::<LE>()? as f32,
			));
		}

		Ok(ret)
	}
}

pub struct SectorData {
	pub floor_height: f32,
	pub ceiling_height: f32,
	pub floor_flat_name: Option<String>,
	pub ceiling_flat_name: Option<String>,
	pub light_level: f32,
	pub special_type: u16,
	pub sector_tag: u16,
}

#[derive(Clone, Copy)]
pub struct SectorsFormat;

impl AssetFormat for SectorsFormat {
	type Asset = Vec<SectorData>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 8))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			let mut buf = [0u8; 8];

			ret.push(SectorData {
				floor_height: reader.read_i16::<LE>()? as f32,
				ceiling_height: reader.read_i16::<LE>()? as f32,
				floor_flat_name: match {
					reader.read_exact(&mut buf)?;
					&buf
				} {
					b"-\0\0\0\0\0\0\0" => None,
					x => Some(std::str::from_utf8(x)?.trim_end_matches('\0').to_owned()),
				},
				ceiling_flat_name: match {
					reader.read_exact(&mut buf)?;
					&buf
				} {
					b"-\0\0\0\0\0\0\0" => None,
					x => Some(std::str::from_utf8(x)?.trim_end_matches('\0').to_owned()),
				},
				light_level: reader.read_u16::<LE>()? as f32 / 255.0,
				special_type: reader.read_u16::<LE>()?,
				sector_tag: reader.read_u16::<LE>()?,
			});
		}

		Ok(ret)
	}
}

#[derive(Clone, Copy)]
pub struct GLVertFormat;

impl AssetFormat for GLVertFormat {
	type Asset = Vec<Vector2<f32>>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 1))?);

		let mut buf = [0u8; 4];
		reader.read_exact(&mut buf)?;

		ensure!(&buf == b"gNd2", "No gNd2 signature found");

		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			ret.push(Vector2::new(
				reader.read_i32::<LE>()? as f32 / 65536.0,
				reader.read_i32::<LE>()? as f32 / 65536.0,
			));
		}

		Ok(ret)
	}
}

pub struct GLSegData {
	pub vertex_indices: [EitherVertex; 2],
	pub linedef_index: Option<usize>,
	pub linedef_side: Side,
	pub partner_seg_index: Option<usize>,
}

pub enum EitherVertex {
	Normal(usize),
	GL(usize),
}

#[derive(Clone, Copy)]
pub struct GLSegsFormat;

impl AssetFormat for GLSegsFormat {
	type Asset = Vec<GLSegData>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 2))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			ret.push(GLSegData {
				vertex_indices: [
					match reader.read_u16::<LE>()? as usize {
						x if x & 0x8000 != 0 => EitherVertex::GL(x & 0x7FFF),
						x => EitherVertex::Normal(x),
					},
					match reader.read_u16::<LE>()? as usize {
						x if x & 0x8000 != 0 => EitherVertex::GL(x & 0x7FFF),
						x => EitherVertex::Normal(x),
					},
				],
				linedef_index: match reader.read_u16::<LE>()? as usize {
					0xFFFF => None,
					x => Some(x),
				},
				linedef_side: match reader.read_u16::<LE>()? as usize {
					0 => Side::Right,
					_ => Side::Left,
				},
				partner_seg_index: match reader.read_u16::<LE>()? as usize {
					0xFFFF => None,
					x => Some(x),
				},
			});
		}

		Ok(ret)
	}
}

pub struct GLSSectData {
	pub seg_count: usize,
	pub first_seg_index: usize,
}

#[derive(Clone, Copy)]
pub struct GLSSectFormat;

impl AssetFormat for GLSSectFormat {
	type Asset = Vec<GLSSectData>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 3))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			ret.push(GLSSectData {
				seg_count: reader.read_u16::<LE>()? as usize,
				first_seg_index: reader.read_u16::<LE>()? as usize,
			});
		}

		Ok(ret)
	}
}

pub struct GLNodeData {
	pub partition_point: Vector2<f32>,
	pub partition_dir: Vector2<f32>,
	pub child_bboxes: [AABB2; 2],
	pub child_indices: [NodeChild; 2],
}

#[derive(Clone, Copy)]
pub struct GLNodesFormat;

impl AssetFormat for GLNodesFormat {
	type Asset = Vec<GLNodeData>;

	fn import(&self, name: &str, source: &impl DataSource) -> anyhow::Result<Self::Asset> {
		let mut reader = Cursor::new(source.load(&format!("{}/+{}", name, 4))?);
		let mut ret = Vec::new();

		while (reader.position() as usize) < reader.get_ref().len() {
			ret.push(GLNodeData {
				partition_point: Vector2::new(
					reader.read_i16::<LE>()? as f32,
					reader.read_i16::<LE>()? as f32,
				),
				partition_dir: Vector2::new(
					reader.read_i16::<LE>()? as f32,
					reader.read_i16::<LE>()? as f32,
				),
				child_bboxes: [
					AABB2::from_extents(
						reader.read_i16::<LE>()? as f32,
						reader.read_i16::<LE>()? as f32,
						reader.read_i16::<LE>()? as f32,
						reader.read_i16::<LE>()? as f32,
					),
					AABB2::from_extents(
						reader.read_i16::<LE>()? as f32,
						reader.read_i16::<LE>()? as f32,
						reader.read_i16::<LE>()? as f32,
						reader.read_i16::<LE>()? as f32,
					),
				],
				child_indices: [
					match reader.read_u16::<LE>()? as usize {
						x if x & 0x8000 != 0 => NodeChild::Subsector(x & 0x7FFF),
						x => NodeChild::Node(x),
					},
					match reader.read_u16::<LE>()? as usize {
						x if x & 0x8000 != 0 => NodeChild::Subsector(x & 0x7FFF),
						x => NodeChild::Node(x),
					},
				],
			});
		}

		Ok(ret)
	}
}
