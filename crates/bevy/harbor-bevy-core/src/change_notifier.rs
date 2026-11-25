use std::marker::PhantomData;

use bevy::prelude::*;

/// Fired in `PreUpdate` when a resource is added or changed.
#[derive(Event)]
pub struct ResourceChanged<R: Resource>(PhantomData<R>);

impl<R: Resource> Default for ResourceChanged<R> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

/// Fired in `PreUpdate` for each entity whose component was added or changed.
#[derive(EntityEvent)]
pub struct ComponentChanged<C: Component> {
    pub entity: Entity,
    component: PhantomData<C>,
}

/// App extension for bridging Bevy change detection to observers.
pub trait AppChangeNotifierExt {
    /// Register a resource notifier. Repeating registration has no effect.
    fn observe_resource_change<R: Resource>(&mut self) -> &mut Self;

    /// Register a component notifier. Repeating registration has no effect.
    fn observe_component_change<C: Component>(&mut self) -> &mut Self;
}

#[derive(Resource)]
struct NotifierRegistered<E: Event>(PhantomData<E>);

impl AppChangeNotifierExt for App {
    fn observe_resource_change<R: Resource>(&mut self) -> &mut Self {
        if !self
            .world()
            .contains_resource::<NotifierRegistered<ResourceChanged<R>>>()
        {
            self.insert_resource(NotifierRegistered::<ResourceChanged<R>>(PhantomData))
                .add_systems(PreUpdate, notify_resource_change::<R>);
        }
        self
    }

    fn observe_component_change<C: Component>(&mut self) -> &mut Self {
        if !self
            .world()
            .contains_resource::<NotifierRegistered<ComponentChanged<C>>>()
        {
            self.insert_resource(NotifierRegistered::<ComponentChanged<C>>(PhantomData))
                .add_systems(PreUpdate, notify_component_change::<C>);
        }
        self
    }
}

fn notify_resource_change<R: Resource>(resource: Option<Res<R>>, mut commands: Commands) {
    if resource.is_some_and(|resource| resource.is_changed()) {
        commands.trigger(ResourceChanged::<R>::default());
    }
}

fn notify_component_change<C: Component>(
    entities: Query<Entity, Changed<C>>,
    mut commands: Commands,
) {
    for entity in &entities {
        commands.trigger(ComponentChanged::<C> {
            entity,
            component: PhantomData,
        });
    }
}
