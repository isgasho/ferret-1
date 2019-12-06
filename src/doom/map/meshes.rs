use crate::{
	assets::{AssetHandle, AssetStorage},
	doom::map::{DoomMap, LinedefFlags, Side},
	renderer::{
		mesh::{Mesh, MeshBuilder},
		texture::Texture,
		video::Video,
	},
};
use nalgebra::Vector2;
use specs::{ReadExpect, SystemData, World};
use std::{collections::HashMap, error::Error};
use vulkano::image::Dimensions;

pub struct MapModel {
	meshes: Vec<(AssetHandle<Texture>, Mesh)>,
	sky_mesh: (AssetHandle<Texture>, Mesh),
}

impl MapModel {
	pub fn new(
		meshes: Vec<(AssetHandle<Texture>, Mesh)>,
		sky_mesh: (AssetHandle<Texture>, Mesh),
	) -> MapModel {
		MapModel { meshes, sky_mesh }
	}

	pub fn meshes(&self) -> &Vec<(AssetHandle<Texture>, Mesh)> {
		&self.meshes
	}

	pub fn sky_mesh(&self) -> &(AssetHandle<Texture>, Mesh) {
		&self.sky_mesh
	}
}

#[derive(Clone, Debug, Default)]
pub struct VertexData {
	pub in_position: [f32; 3],
	pub in_texture_coord: [f32; 3],
	pub in_lightlevel: f32,
}
impl_vertex!(VertexData, in_position, in_texture_coord, in_lightlevel);

#[derive(Clone, Debug, Default)]
pub struct SkyVertexData {
	pub in_position: [f32; 3],
}
impl_vertex!(SkyVertexData, in_position);

pub fn make_model(
	map_data: &DoomMap,
	sky: AssetHandle<Texture>,
	world: &World,
) -> Result<MapModel, Box<dyn Error>> {
	// Load textures and flats
	let [textures, flats] = super::textures::load_textures(map_data, world)?;

	// Create meshes
	let (meshes, sky_mesh) = make_meshes(map_data, &textures, &flats, world)?;
	let mut ret = Vec::new();

	let video = world.fetch::<Video>();

	// Regular meshes
	for (tex, (vertices, indices)) in meshes {
		let (mesh, future) = MeshBuilder::new()
			.with_vertices(vertices)
			.with_indices(indices)
			.build(&video.queues().graphics)?;

		ret.push((tex, mesh));
	}

	// Sky mesh
	let (vertices, indices) = sky_mesh;
	let (mesh, future) = MeshBuilder::new()
		.with_vertices(vertices)
		.with_indices(indices)
		.build(&video.queues().graphics)?;

	Ok(MapModel::new(ret, (sky, mesh)))
}

fn make_meshes(
	map: &DoomMap,
	textures: &HashMap<String, (AssetHandle<Texture>, usize)>,
	flats: &HashMap<String, (AssetHandle<Texture>, usize)>,
	world: &World,
) -> Result<
	(
		HashMap<AssetHandle<Texture>, (Vec<VertexData>, Vec<u32>)>,
		(Vec<SkyVertexData>, Vec<u32>),
	),
	Box<dyn Error>,
> {
	#[inline]
	fn push_wall(
		vertices: &mut Vec<VertexData>,
		indices: &mut Vec<u32>,
		vert_h: [Vector2<f32>; 2],
		vert_v: [f32; 2],
		tex_v: [f32; 2],
		offset: Vector2<f32>,
		dimensions: Dimensions,
		texture_layer: f32,
		light_level: f32,
	) {
		let width = (vert_h[1] - vert_h[0]).norm();
		indices.push(u32::max_value());

		for (h, v) in [(1, 0), (0, 0), (0, 1), (1, 1)].iter().copied() {
			indices.push(vertices.len() as u32);
			vertices.push(VertexData {
				in_position: [vert_h[h][0], vert_h[h][1], vert_v[v]],
				in_texture_coord: [
					(offset[0] + width * h as f32) / dimensions.width() as f32,
					(offset[1] + tex_v[v]) / dimensions.height() as f32,
					texture_layer,
				],
				in_lightlevel: light_level,
			});
		}
	}

	#[inline]
	fn push_sky_wall(
		vertices: &mut Vec<SkyVertexData>,
		indices: &mut Vec<u32>,
		vert_h: [Vector2<f32>; 2],
		vert_v: [f32; 2],
	) {
		indices.push(u32::max_value());

		for (h, v) in [(1, 0), (0, 0), (0, 1), (1, 1)].iter().copied() {
			indices.push(vertices.len() as u32);
			vertices.push(SkyVertexData {
				in_position: [vert_h[h][0], vert_h[h][1], vert_v[v]],
			});
		}
	}

	#[inline]
	fn push_flat(
		vertices: &mut Vec<VertexData>,
		indices: &mut Vec<u32>,
		iter: impl Iterator<Item = Vector2<f32>>,
		vert_z: f32,
		dimensions: Dimensions,
		texture_layer: f32,
		light_level: f32,
	) {
		indices.push(u32::max_value());

		for vert in iter {
			indices.push(vertices.len() as u32);
			vertices.push(VertexData {
				in_position: [vert[0], vert[1], vert_z],
				in_texture_coord: [
					vert[0] / dimensions.width() as f32,
					vert[1] / dimensions.height() as f32,
					texture_layer,
				],
				in_lightlevel: light_level,
			});
		}
	}

	#[inline]
	fn push_sky_flat(
		vertices: &mut Vec<SkyVertexData>,
		indices: &mut Vec<u32>,
		iter: impl Iterator<Item = Vector2<f32>>,
		vert_z: f32,
	) {
		indices.push(u32::max_value());

		for vert in iter {
			indices.push(vertices.len() as u32);
			vertices.push(SkyVertexData {
				in_position: [vert[0], vert[1], vert_z],
			});
		}
	}

	let mut meshes: HashMap<AssetHandle<Texture>, (Vec<VertexData>, Vec<u32>)> = HashMap::new();
	let mut sky_mesh: (Vec<SkyVertexData>, Vec<u32>) = (Vec::new(), Vec::new());
	let texture_storage = <ReadExpect<AssetStorage<Texture>>>::fetch(world);

	// Walls
	for linedef in &map.linedefs {
		for side in [Side::Right, Side::Left].iter().copied() {
			let front_sidedef = match linedef.sidedef_indices[side as usize] {
				Some(front_sidedef_index) => &map.sidedefs[front_sidedef_index],
				None => continue,
			};
			let front_sector = &map.sectors[front_sidedef.sector_index];

			// Swap the vertices if we're on the left side of the linedef
			let linedef_vertices = match side {
				Side::Right => linedef.vertices,
				Side::Left => [linedef.vertices[1], linedef.vertices[0]],
			};

			// Two-sided or one-sided sidedef?
			if let Some(back_sidedef_index) = linedef.sidedef_indices[!side as usize] {
				let back_sidedef = &map.sidedefs[back_sidedef_index];
				let back_sector = &map.sectors[back_sidedef.sector_index];
				let spans = [
					front_sector.ceiling_height,
					f32::min(front_sector.ceiling_height, back_sector.ceiling_height),
					f32::max(back_sector.floor_height, front_sector.floor_height),
					front_sector.floor_height,
				];

				// Top section
				if front_sector.ceiling_flat_name == "F_SKY1"
					&& back_sector.ceiling_flat_name == "F_SKY1"
				{
					push_sky_wall(
						&mut sky_mesh.0,
						&mut sky_mesh.1,
						linedef_vertices,
						[spans[0], spans[1]],
					);
				} else if let Some(texture_name) = &front_sidedef.top_texture_name {
					let texture = &textures[texture_name];
					let dimensions = texture_storage.get(&texture.0).unwrap().dimensions();
					let (ref mut vertices, ref mut indices) =
						meshes.entry(texture.0.clone()).or_insert((vec![], vec![]));

					let tex_v = if linedef.flags.contains(LinedefFlags::DONTPEGTOP) {
						[0.0, spans[0] - spans[1]]
					} else {
						[spans[1] - spans[0], 0.0]
					};

					push_wall(
						vertices,
						indices,
						linedef_vertices,
						[spans[0], spans[1]],
						tex_v,
						front_sidedef.texture_offset,
						dimensions,
						texture.1 as f32,
						front_sector.light_level,
					);
				}

				// Bottom section
				if let Some(texture_name) = &front_sidedef.bottom_texture_name {
					let texture = &textures[texture_name];
					let dimensions = texture_storage.get(&texture.0).unwrap().dimensions();
					let (ref mut vertices, ref mut indices) =
						meshes.entry(texture.0.clone()).or_insert((vec![], vec![]));

					let tex_v = if linedef.flags.contains(LinedefFlags::DONTPEGBOTTOM) {
						[
							front_sector.ceiling_height - spans[2],
							front_sector.ceiling_height - spans[3],
						]
					} else {
						[0.0, spans[2] - spans[3]]
					};

					push_wall(
						vertices,
						indices,
						linedef_vertices,
						[spans[2], spans[3]],
						tex_v,
						front_sidedef.texture_offset,
						dimensions,
						texture.1 as f32,
						front_sector.light_level,
					);
				}

				// Middle section
				if let Some(texture_name) = &front_sidedef.middle_texture_name {
					let texture = &textures[texture_name];
					let dimensions = texture_storage.get(&texture.0).unwrap().dimensions();
					let (ref mut vertices, ref mut indices) =
						meshes.entry(texture.0.clone()).or_insert((vec![], vec![]));

					let tex_v = if linedef.flags.contains(LinedefFlags::DONTPEGBOTTOM) {
						[spans[2] - spans[1], 0.0]
					} else {
						[0.0, spans[1] - spans[2]]
					};

					push_wall(
						vertices,
						indices,
						linedef_vertices,
						[spans[1], spans[2]],
						tex_v,
						front_sidedef.texture_offset,
						dimensions,
						texture.1 as f32,
						front_sector.light_level,
					);
				}
			} else {
				if let Some(texture_name) = &front_sidedef.middle_texture_name {
					let texture = &textures[texture_name];
					let dimensions = texture_storage.get(&texture.0).unwrap().dimensions();
					let (ref mut vertices, ref mut indices) =
						meshes.entry(texture.0.clone()).or_insert((vec![], vec![]));

					let tex_v = if linedef.flags.contains(LinedefFlags::DONTPEGBOTTOM) {
						[front_sector.floor_height - front_sector.ceiling_height, 0.0]
					} else {
						[0.0, front_sector.ceiling_height - front_sector.floor_height]
					};

					push_wall(
						vertices,
						indices,
						linedef_vertices,
						[front_sector.ceiling_height, front_sector.floor_height],
						tex_v,
						front_sidedef.texture_offset,
						dimensions,
						texture.1 as f32,
						front_sector.light_level,
					);
				}
			}
		}
	}

	// Flats
	for ssect in &map.gl_ssect {
		let sector = &map.sectors[ssect.sector_index];
		let segs = &map.gl_segs[ssect.first_seg_index..ssect.first_seg_index + ssect.seg_count];

		// Floor
		let iter = segs.iter().rev().map(|seg| seg.vertices[0]);

		if sector.floor_flat_name == "F_SKY1" {
			push_sky_flat(&mut sky_mesh.0, &mut sky_mesh.1, iter, sector.floor_height);
		} else {
			let flat = &flats[&sector.floor_flat_name];
			let dimensions = texture_storage.get(&flat.0).unwrap().dimensions();
			let (ref mut vertices, ref mut indices) =
				meshes.entry(flat.0.clone()).or_insert((vec![], vec![]));

			push_flat(
				vertices,
				indices,
				iter,
				sector.floor_height,
				dimensions,
				flat.1 as f32,
				sector.light_level,
			);
		};

		// Ceiling
		let iter = segs.iter().map(|seg| seg.vertices[0]);

		if sector.ceiling_flat_name == "F_SKY1" {
			push_sky_flat(
				&mut sky_mesh.0,
				&mut sky_mesh.1,
				iter,
				sector.ceiling_height,
			);
		} else {
			let flat = &flats[&sector.ceiling_flat_name];
			let dimensions = texture_storage.get(&flat.0).unwrap().dimensions();
			let (ref mut vertices, ref mut indices) =
				meshes.entry(flat.0.clone()).or_insert((vec![], vec![]));

			push_flat(
				vertices,
				indices,
				iter,
				sector.ceiling_height,
				dimensions,
				flat.1 as f32,
				sector.light_level,
			);
		}
	}

	Ok((meshes, sky_mesh))
}