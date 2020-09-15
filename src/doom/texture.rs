use crate::{
	common::assets::AssetStorage,
	doom::map::{LinedefRef, MapDynamic},
};
use legion::{systems::Runnable, IntoQuery, SystemBuilder};
use nalgebra::Vector2;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct TextureScroll {
	pub speed: Vector2<f32>,
}

pub fn texture_animation_system() -> impl Runnable {
	SystemBuilder::new("texture_animation_system")
		.read_resource::<AssetStorage>()
		.read_resource::<Duration>()
		.with_query(<&mut MapDynamic>::query())
		.build(move |_, world, resources, query| {
			let (asset_storage, delta) = resources;

			for map_dynamic in query.iter_mut(world) {
				for (handle, anim_state) in map_dynamic.anim_states.iter_mut() {
					anim_state.timer.tick(**delta);

					if anim_state.timer.is_zero() {
						let map = asset_storage.get(&map_dynamic.map).unwrap();
						let anim = &map.anims[handle];
						anim_state.frame = (anim_state.frame + 1) % anim.frames.len();
						anim_state.timer.reset();
					}
				}
			}
		})
}

pub fn texture_scroll_system() -> impl Runnable {
	SystemBuilder::new("texture_scroll_system")
		.read_resource::<Duration>()
		.with_query(<(&LinedefRef, &TextureScroll)>::query())
		.with_query(<&mut MapDynamic>::query())
		.build(move |_, world, delta, queries| {
			let (world0, mut world) = world.split_for_query(&queries.0);

			// Scroll textures
			for (linedef_ref, texture_scroll) in queries.0.iter(&world0) {
				let map_dynamic = queries
					.1
					.get_mut(&mut world, linedef_ref.map_entity)
					.unwrap();
				let linedef_dynamic = &mut map_dynamic.linedefs[linedef_ref.index];
				linedef_dynamic.texture_offset += texture_scroll.speed * delta.as_secs_f32();
			}
		})
}
