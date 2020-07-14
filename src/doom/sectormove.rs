use crate::{
	assets::{AssetHandle, AssetStorage},
	audio::Sound,
	doom::{
		components::Transform,
		map::{MapDynamic, SectorRef},
		physics::{BoxCollider, SectorPushTracer},
	},
	quadtree::Quadtree,
	timer::Timer,
};
use legion::prelude::{
	Entity, EntityStore, IntoQuery, Read, Resources, Runnable, SystemBuilder, Write,
};
use shrev::EventChannel;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct SectorMove {
	pub velocity: f32,
	pub target: f32,
	pub sound: Option<AssetHandle<Sound>>,
	pub sound_timer: Timer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectorMoveEvent {
	pub event_type: SectorMoveEventType,
	pub entity: Entity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectorMoveEventType {
	Collided,
	TargetReached,
}

pub fn sector_move_system(resources: &mut Resources) -> Box<dyn Runnable> {
	resources.insert(EventChannel::<SectorMoveEvent>::new());

	SystemBuilder::new("sector_move_system")
		.read_resource::<AssetStorage>()
		.read_resource::<Duration>()
		.read_resource::<Quadtree>()
		.write_resource::<EventChannel<SectorMoveEvent>>()
		.write_resource::<Vec<(AssetHandle<Sound>, Entity)>>()
		.with_query(<(Read<SectorRef>, Write<SectorMove>)>::query())
		.read_component::<BoxCollider>() // used by SectorTracer
		.write_component::<MapDynamic>()
		.write_component::<Transform>()
		.build_thread_local(move |_, world, resources, query| {
			let (asset_storage, delta, quadtree, sector_move_event_channel, sound_queue) =
				resources;
			let (mut map_dynamic_world, mut world) = world.split::<Write<MapDynamic>>();
			let (mut query_world, mut world) = world.split_for_query(&query);

			for (entity, (sector_ref, mut sector_move)) in query.iter_entities_mut(&mut query_world)
			{
				if sector_move.velocity != 0.0 {
					let mut map_dynamic = map_dynamic_world
						.get_component_mut::<MapDynamic>(sector_ref.map_entity)
						.unwrap();
					let map = asset_storage.get(&map_dynamic.map).unwrap();
					let sector = &map.sectors[sector_ref.index];
					let mut event_type = None;

					sector_move.sound_timer.tick(**delta);

					if sector_move.sound_timer.is_zero() && sector_move.sound.is_some() {
						sector_move.sound_timer.reset();
						sound_queue.push((sector_move.sound.as_ref().unwrap().clone(), entity));
					}

					let mut move_step = sector_move.velocity * delta.as_secs_f32();
					let current_height = map_dynamic.sectors[sector_ref.index].interval.min;
					let distance_left = sector_move.target - current_height;

					if move_step < 0.0 {
						if move_step <= distance_left {
							move_step = distance_left;
							event_type = Some(SectorMoveEventType::TargetReached);
						}
					} else if move_step > 0.0 {
						if move_step >= distance_left {
							move_step = distance_left;
							event_type = Some(SectorMoveEventType::TargetReached);
						}
					}

					let tracer = SectorPushTracer {
						map,
						map_dynamic: &map_dynamic,
						quadtree,
						world: &world,
					};

					let trace = tracer.trace(
						current_height,
						1.0,
						move_step,
						sector.subsectors.iter().map(|i| &map.subsectors[*i]),
					);

					// Push the entities out of the way
					for pushed_entity in trace.pushed_entities.iter() {
						let mut transform = world
							.get_component_mut::<Transform>(pushed_entity.entity)
							.unwrap();
						transform.position += pushed_entity.move_step;
					}

					// Move the plat into place
					let sector_dynamic = &mut map_dynamic.sectors[sector_ref.index];
					sector_dynamic.interval.min += trace.move_step;

					if trace.fraction < 1.0 {
						event_type = Some(SectorMoveEventType::Collided);
					} else if event_type == Some(SectorMoveEventType::TargetReached) {
						// Set this explicitly to the exact value
						let sector_dynamic = &mut map_dynamic.sectors[sector_ref.index];
						sector_dynamic.interval.min = sector_move.target;
					}

					if let Some(event_type) = event_type {
						sector_move_event_channel
							.single_write(SectorMoveEvent { entity, event_type });
					}
				}
			}
		})
}