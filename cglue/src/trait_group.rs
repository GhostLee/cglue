//! Core definitions for traits, and their groups.

// TODO: split everything up

use crate::boxed::CBox;
#[cfg(not(feature = "rust_void"))]
pub use core::ffi::c_void;
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
#[cfg(feature = "rust_void")]
#[allow(non_camel_case_types)]
pub type c_void = ();

/// Simple CGlue trait object.
///
/// This is the simplest form of CGlue object, represented by a container and vtable for a single
/// trait.
///
/// Container merely is a this pointer with some optional temporary return reference context.
#[repr(C)]
pub struct CGlueTraitObj<'a, T, V, C, R> {
    vtbl: &'a V,
    container: CGlueObjContainer<T, C, R>,
}

/// Simple CGlue trait object container.
///
/// This is the simplest form of container, represented by an instance, clone context, and
/// temporary return context.
///
/// `instance` value usually is either a reference, or a mutable reference, or a `CBox`, which
/// contains static reference to the instance, and a dedicated drop function for freeing resources.
///
/// `context` is either `PhantomData` representing nothing, or typically a `CArc` that can be
/// cloned at will, reference counting some resource, like a `Library` for automatic unloading.
///
/// `ret_tmp` is usually `PhantomData` representing nothing, unless the trait has functions that
/// return references to associated types, in which case space is reserved for wrapping structures.
#[repr(C)]
pub struct CGlueObjContainer<T, C, R> {
    instance: T,
    context: C,
    ret_tmp: R,
}

/// Describes an opaquable object.
///
/// This trait provides a safe many-traits-to-one conversion. For instance, concrete vtable types
/// get converted to `c_void` types, and so on.
///
/// # Safety
///
/// Implementor of this trait must ensure the same layout of regular and opaque data. Generally,
/// this means using the same structure, but taking type T and converting it to c_void, but it is
/// not limited to that.
///
/// In addition, it is key to know that any functions on the type that expect a concrete type
/// parameter become undefined behaviour. For instance, moving out of a opaque `Box` is
/// undefined behaviour.
pub unsafe trait Opaquable: Sized {
    type OpaqueTarget;

    /// Transform self into an opaque version of the trait object.
    ///
    /// The opaque version safely destroys type information, and after this point there is no way
    /// back.
    fn into_opaque(self) -> Self::OpaqueTarget {
        // Implementors should ensure the same size.
        debug_assert_eq!(
            core::mem::size_of::<Self>(),
            core::mem::size_of::<Self::OpaqueTarget>()
        );

        let input = ManuallyDrop::new(self);

        // We could use a union here, but that forbids us from using Rust 1.45.
        // Rust does optimize this into a no-op anyways
        unsafe { core::ptr::read(&input as *const _ as *const _) }
    }
}

pub trait Opaque: Sized + Opaquable<OpaqueTarget = Self> {}
impl<T: Opaquable<OpaqueTarget = T>> Opaque for T {}

unsafe impl<T: Opaquable, C, R> Opaquable for CGlueObjContainer<T, C, R> {
    type OpaqueTarget = CGlueObjContainer<T::OpaqueTarget, C, R>;
}

unsafe impl<'a, T> Opaquable for &'a T {
    type OpaqueTarget = &'a c_void;
}

unsafe impl<'a, T> Opaquable for &'a mut T {
    type OpaqueTarget = &'a mut c_void;
}

/// Opaque type of the trait object.
pub type CGlueOpaqueTraitObjOutCBox<'a, V> = CGlueTraitObj<
    'a,
    CBox<'a, c_void>,
    <V as CGlueBaseVtbl>::OpaqueVtbl,
    <V as CGlueBaseVtbl>::Context,
    <V as CGlueBaseVtbl>::RetTmp,
>;

pub type CGlueOpaqueTraitObjOutRef<'a, V> = CGlueTraitObj<
    'a,
    &'a c_void,
    <V as CGlueBaseVtbl>::OpaqueVtbl,
    <V as CGlueBaseVtbl>::Context,
    <V as CGlueBaseVtbl>::RetTmp,
>;

pub type CGlueOpaqueTraitObjOutMut<'a, V> = CGlueTraitObj<
    'a,
    &'a mut c_void,
    <V as CGlueBaseVtbl>::OpaqueVtbl,
    <V as CGlueBaseVtbl>::Context,
    <V as CGlueBaseVtbl>::RetTmp,
>;

pub type CGlueOpaqueTraitObj<'a, T, V> = CGlueTraitObj<
    'a,
    <T as Opaquable>::OpaqueTarget,
    <V as CGlueBaseVtbl>::OpaqueVtbl,
    <V as CGlueBaseVtbl>::Context,
    <V as CGlueBaseVtbl>::RetTmp,
>;

unsafe impl<
        'a,
        T: Opaquable,
        F: CGlueBaseVtbl<Context = C, RetTmp = R>,
        C: Clone + Send + Sync,
        R: Default,
    > Opaquable for CGlueTraitObj<'a, T, F, C, R>
{
    type OpaqueTarget = CGlueTraitObj<'a, T::OpaqueTarget, F::OpaqueVtbl, C, R>;
}

pub trait GetVtbl<V> {
    fn get_vtbl(&self) -> &V;
}

impl<T, V, C, R> GetVtbl<V> for CGlueTraitObj<'_, T, V, C, R> {
    fn get_vtbl(&self) -> &V {
        self.vtbl
    }
}

// Conversions into container type itself.
// Needed when generated code returns Self

impl<'a, T: Deref<Target = F>, F, C: 'static + Clone + Send + Sync, R: Default> From<(T, C)>
    for CGlueObjContainer<T, C, R>
{
    fn from((instance, context): (T, C)) -> Self {
        Self {
            instance,
            ret_tmp: Default::default(),
            context,
        }
    }
}

impl<'a, T: Deref<Target = F>, F, R: Default> From<T> for CGlueObjContainer<T, NoContext, R> {
    fn from(this: T) -> Self {
        Self::from((this, Default::default()))
    }
}

impl<'a, T, R: Default> From<T> for CGlueObjContainer<CBox<'a, T>, NoContext, R> {
    fn from(this: T) -> Self {
        Self::from(CBox::from(this))
    }
}

impl<'a, T, C: 'static + Clone + Send + Sync, R: Default> From<(T, C)>
    for CGlueObjContainer<CBox<'a, T>, C, R>
{
    fn from((this, context): (T, C)) -> Self {
        Self::from((CBox::from(this), context))
    }
}

impl<
        'a,
        T: Deref<Target = F>,
        F,
        V: CGlueVtbl<CGlueObjContainer<T, C, R>, Context = C, RetTmp = R>,
        C: 'static + Clone + Send + Sync,
        R: Default,
    > From<CGlueObjContainer<T, C, R>> for CGlueTraitObj<'a, T, V, V::Context, V::RetTmp>
where
    &'a V: Default,
{
    fn from(container: CGlueObjContainer<T, C, R>) -> Self {
        Self {
            container,
            vtbl: Default::default(),
        }
    }
}

impl<
        'a,
        T: Deref<Target = F>,
        F,
        V: CGlueVtbl<CGlueObjContainer<T, C, R>, Context = C, RetTmp = R>,
        C: 'static + Clone + Send + Sync,
        R: Default,
    > From<(T, V::Context)> for CGlueTraitObj<'a, T, V, V::Context, V::RetTmp>
where
    &'a V: Default,
{
    fn from((instance, context): (T, V::Context)) -> Self {
        Self::from(CGlueObjContainer::from((instance, context)))
    }
}

impl<
        'a,
        T: Deref<Target = F>,
        F,
        V: CGlueVtbl<CGlueObjContainer<T, NoContext, R>, Context = NoContext, RetTmp = R>,
        R: Default,
    > From<T> for CGlueTraitObj<'a, T, V, V::Context, V::RetTmp>
where
    &'a V: Default,
{
    fn from(this: T) -> Self {
        Self::from((this, Default::default()))
    }
}

impl<
        'a,
        T,
        V: CGlueVtbl<CGlueObjContainer<CBox<'a, T>, NoContext, R>, Context = NoContext, RetTmp = R>,
        R: Default,
    > From<T> for CGlueTraitObj<'a, CBox<'a, T>, V, V::Context, V::RetTmp>
where
    &'a V: Default,
{
    fn from(this: T) -> Self {
        Self::from(CBox::from(this))
    }
}

impl<
        'a,
        T,
        V: CGlueVtbl<CGlueObjContainer<CBox<'a, T>, C, R>, Context = C, RetTmp = R>,
        C: 'static + Clone + Send + Sync,
        R: Default,
    > From<(T, V::Context)> for CGlueTraitObj<'a, CBox<'a, T>, V, V::Context, V::RetTmp>
where
    &'a V: Default,
{
    fn from((this, context): (T, V::Context)) -> Self {
        Self::from((CBox::from(this), context))
    }
}

/// CGlue compatible object.
///
/// This trait allows to retrieve the constant `this` pointer on the structure.
pub trait CGlueObjBase {
    /// Type of the underlying object.
    type ObjType;
    /// Type of the container housing the object.
    type InstType: ::core::ops::Deref<Target = Self::ObjType>;
    /// Type of the context associated with the container.
    type Context: 'static + Clone + Send + Sync;

    fn cobj_base_ref(&self) -> (&Self::ObjType, &Self::Context);
    fn cobj_base_owned(self) -> (Self::InstType, Self::Context);
}

pub trait CGlueObjRef<R>: CGlueObjBase {
    fn cobj_ref(&self) -> (&Self::ObjType, &R, &Self::Context);
}

impl<T: Deref<Target = F>, F, C: 'static + Clone + Send + Sync, R> CGlueObjBase
    for CGlueObjContainer<T, C, R>
{
    type ObjType = F;
    type InstType = T;
    type Context = C;

    fn cobj_base_ref(&self) -> (&F, &Self::Context) {
        (self.instance.deref(), &self.context)
    }

    fn cobj_base_owned(self) -> (T, Self::Context) {
        (self.instance, self.context)
    }
}

impl<T: Deref<Target = F>, F, C: 'static + Clone + Send + Sync, R> CGlueObjRef<R>
    for CGlueObjContainer<T, C, R>
{
    fn cobj_ref(&self) -> (&F, &R, &Self::Context) {
        (self.instance.deref(), &self.ret_tmp, &self.context)
    }
}

/// CGlue compatible object.
///
/// This trait allows to retrieve the mutable `this` pointer on the structure.
pub trait CGlueObjMut<R>: CGlueObjRef<R> {
    fn cobj_mut(&mut self) -> (&mut Self::ObjType, &mut R, &Self::Context);
}

impl<T: Deref<Target = F> + DerefMut, F, C: 'static + Clone + Send + Sync, R> CGlueObjMut<R>
    for CGlueObjContainer<T, C, R>
{
    fn cobj_mut(&mut self) -> (&mut F, &mut R, &Self::Context) {
        (self.instance.deref_mut(), &mut self.ret_tmp, &self.context)
    }
}

pub trait GetContainer {
    type ContType: CGlueObjBase;

    fn ccont_ref(&self) -> &Self::ContType;
    fn ccont_mut(&mut self) -> &mut Self::ContType;
    fn into_ccont(self) -> Self::ContType;
    fn build_with_ccont(&self, container: Self::ContType) -> Self;
}

impl<T: Deref<Target = F>, F, V, C: 'static + Clone + Send + Sync, R> GetContainer
    for CGlueTraitObj<'_, T, V, C, R>
{
    type ContType = CGlueObjContainer<T, C, R>;

    fn ccont_ref(&self) -> &Self::ContType {
        &self.container
    }

    fn ccont_mut(&mut self) -> &mut Self::ContType {
        &mut self.container
    }

    fn into_ccont(self) -> Self::ContType {
        self.container
    }

    fn build_with_ccont(&self, container: Self::ContType) -> Self {
        Self {
            container,
            vtbl: self.vtbl,
        }
    }
}

/// Convert a container into inner type.
pub trait IntoInner {
    type InnerTarget;

    /// Consume self and return inner type.
    ///
    /// # Safety
    ///
    /// It might be unsafe to invoke this method if the container has an opaque type, or is on
    /// the wrong side of FFI. CGlue code generator guards against these problems, but it is
    /// important to consider them when working manually with this trait.
    unsafe fn into_inner(self) -> Self::InnerTarget;
}

/// Trait for CGlue vtables.
pub trait CGlueVtbl<T>: CGlueBaseVtbl {}

/// Trait for CGlue vtables.
///
/// # Safety
///
/// This trait is meant to be implemented by the code generator. If implementing manually, make
/// sure that the `OpaqueVtbl` is the exact same type, with the only difference being `this` types.
pub unsafe trait CGlueBaseVtbl: Sized {
    type OpaqueVtbl: Sized;
    type Context: Sized + Clone + Send + Sync;
    type RetTmp: Sized + Default;

    /// Get the opaque vtable for the type.
    fn as_opaque(&self) -> &Self::OpaqueVtbl {
        unsafe { &*(self as *const Self as *const Self::OpaqueVtbl) }
    }
}

/// Describes absence of a context.
///
/// This context is used by default whenever a specific context was not supplied.
pub type NoContext = std::marker::PhantomData<c_void>;

unsafe impl<T: Opaquable> Opaquable for std::marker::PhantomData<T> {
    type OpaqueTarget = std::marker::PhantomData<T::OpaqueTarget>;
}

#[cfg(not(feature = "rust_void"))]
unsafe impl Opaquable for () {
    type OpaqueTarget = ();
}

unsafe impl Opaquable for c_void {
    type OpaqueTarget = c_void;
}
