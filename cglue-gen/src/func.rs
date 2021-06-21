use super::generics::{GenericType, ParsedGenerics};
use proc_macro2::TokenStream;
use quote::*;
use std::collections::BTreeMap;
use syn::{Type, *};

const FN_PREFIX: &str = "cglue_wrapped_";

pub struct WrappedType {
    pub ty: GenericType,
    pub ty_static: Option<GenericType>,
    pub lifetime_bound: Option<Lifetime>,
    pub lifetime_type_bound: Option<Lifetime>,
    pub other_bounds: Option<TokenStream>,
    pub other_bounds_simple: Option<TokenStream>,
    pub return_conv: Option<ExprClosure>,
    pub impl_return_conv: Option<TokenStream>,
    pub inject_ret_tmp: bool,
    pub unbounded_hrtb: bool,
    pub needs_ctx: bool,
}

/// TraitArg stores implementations for Unstable-C-Unstable ABI transitions.
struct TraitArg {
    arg: FnArg,
    /// Called in trait impl to define arguments. Useful when need to destruct a tuple/struct.
    to_c_args: TokenStream,
    /// Arguments inside the call to the C vtable function.
    call_c_args: TokenStream,
    /// C function signature.
    c_args: TokenStream,
    /// C function signature, where 'cglue_a lifetimes are replaced with 'cglue_b.
    c_cast_args: TokenStream,
    /// Arguments inside the call to the trait function.
    to_trait_arg: TokenStream,
    /// Whether argument conversion is trivial - 1-1 relationship with no changed types
    trivial: bool,
}

fn wrap_type<'a>(
    ty: &mut Type,
    targets: &'a BTreeMap<Ident, WrappedType>,
) -> Option<(Type, Ident, &'a WrappedType)> {
    match ty {
        Type::Reference(r) => wrap_type(&mut *r.elem, targets),
        Type::Slice(s) => wrap_type(&mut *s.elem, targets),
        Type::Path(p) => {
            let mut iter = p.path.segments.iter();
            match (&p.qself, p.path.leading_colon, iter.next(), iter.next()) {
                (None, None, Some(p1), Some(p2)) => {
                    if p1.ident == "Self" {
                        if let Some(wrapped) = targets.get(&p2.ident) {
                            let WrappedType { ty: new_ty, .. } = wrapped;

                            std::mem::drop(iter);

                            let ident = p2.ident.clone();

                            let ret = std::mem::replace(
                                ty,
                                syn::parse2(new_ty.to_token_stream())
                                    .expect("Failed to parse wrap_type"),
                            );

                            return Some((ret, ident, wrapped));
                        }
                    }
                }
                (None, None, Some(p1), None) => {
                    if p1.ident == "Self" {
                        let self_return_wrap =
                            targets.get(&p1.ident).expect("No self-wrap rule specified");
                        let WrappedType { ty: new_ty, .. } = self_return_wrap;

                        std::mem::drop(iter);

                        let ident = p1.ident.clone();

                        let ret = std::mem::replace(
                            ty,
                            syn::parse2(new_ty.to_token_stream())
                                .expect("Failed to parse self-type wrap"),
                        );

                        return Some((ret, ident, self_return_wrap));
                    }
                }
                _ => {}
            }

            std::mem::drop(iter);

            for seg in p.path.segments.iter_mut() {
                if let PathArguments::AngleBracketed(brac) = &mut seg.arguments {
                    for arg in brac.args.iter_mut() {
                        if let GenericArgument::Type(ty) = arg {
                            let ret = wrap_type(ty, targets);
                            if ret.is_some() {
                                return ret;
                            }
                        }
                    }
                }
            }

            None
        }
        Type::Ptr(ptr) => wrap_type(&mut *ptr.elem, targets),
        Type::Tuple(tup) => tup
            .elems
            .iter_mut()
            .filter_map(|e| wrap_type(e, targets))
            .next(),
        // TODO: Other types
        _ => None,
    }
}

impl TraitArg {
    fn new(
        mut arg: FnArg,
        targets: &BTreeMap<Ident, WrappedType>,
        crate_path: &TokenStream,
        inject_lifetime: Option<&Lifetime>,
        inject_lifetime_cast: Option<&Lifetime>,
    ) -> Self {
        let (to_c_args, call_c_args, c_args, c_cast_args, to_trait_arg, trivial) = match &mut arg {
            FnArg::Receiver(r) => {
                let lifetime = inject_lifetime.or_else(|| r.lifetime());
                let lifetime_cast = inject_lifetime_cast.or_else(|| r.lifetime());

                if r.reference.is_none() {
                    (
                        quote!(let thisobj = self.cobj_owned();),
                        quote!(thisobj,),
                        quote!(thisobj: CGlueT,),
                        quote!(thisobj: CGlueT,),
                        quote!(),
                        false,
                    )
                } else if r.mutability.is_some() {
                    (
                        quote!(let (thisptr, ret_tmp, cglue_ctx) = self.cobj_mut();),
                        quote!(thisptr,),
                        quote!(thisptr: &#lifetime mut CGlueF,),
                        quote!(thisptr: &#lifetime_cast mut CGlueF,),
                        quote!(),
                        true,
                    )
                } else {
                    (
                        quote!(let (thisptr, ret_tmp, cglue_ctx) = self.cobj_ref();),
                        quote!(thisptr,),
                        quote!(thisptr: &#lifetime CGlueF,),
                        quote!(thisptr: &#lifetime_cast CGlueF,),
                        quote!(),
                        true,
                    )
                }
            }
            FnArg::Typed(t) => {
                let _old = wrap_type(&mut *t.ty, targets);

                let name = &*t.pat;

                // TODO: handle more edge cases here
                let name = match name {
                    Pat::Ident(id) => id.ident.to_token_stream(),
                    x => quote!(#x),
                };

                let ty = &*t.ty;

                let mut ret = None;

                // TODO: deal with nested conversion
                //if let (Some(old), Type::Path(p)) = {
                //}

                match ty {
                    Type::Reference(r) => {
                        let is_mut = r.mutability.is_some();
                        let new_ty = match &*r.elem {
                            Type::Slice(s) => {
                                let ty = &*s.elem;
                                Some(if is_mut {
                                    quote!(#crate_path::slice::CSliceMut<#ty>)
                                } else {
                                    quote!(#crate_path::slice::CSliceRef<#ty>)
                                })
                            }
                            Type::Path(p) => {
                                if let Some("str") =
                                    p.path.get_ident().map(|i| i.to_string()).as_deref()
                                {
                                    Some(if is_mut {
                                        quote!(#crate_path::slice::CSliceMut<u8>)
                                    } else {
                                        quote!(#crate_path::slice::CSliceRef<u8>)
                                    })
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };

                        if let Some(slty) = new_ty {
                            ret = Some((
                                quote!(),
                                quote!(#name.into(),),
                                quote!(#name: #slty,),
                                quote!(#name: #slty,),
                                quote!(#name.into(),),
                                false,
                            ))
                        }
                    }
                    // TODO: Warn if Box is being used.
                    Type::Path(p) => {
                        // Here we check for any Option types, and wrap them to COption if they can
                        // not be NPOd.
                        let last = p.path.segments.last();
                        if let Some((PathArguments::AngleBracketed(args), last)) =
                            last.map(|l| (&l.arguments, l))
                        {
                            match last.ident.to_string().as_str() {
                                "Option" => {
                                    if let Some(GenericArgument::Type(a)) = args.args.first() {
                                        if !crate::util::is_null_pointer_optimizable(a, &[]) {
                                            ret = Some((
                                                quote!(let #name = #name.into();),
                                                quote!(#name,),
                                                quote!(#name: #crate_path::option::COption<#a>,),
                                                quote!(#name: #crate_path::option::COption<#a>,),
                                                quote!(#name.into(),),
                                                false,
                                            ));
                                        }
                                    }
                                }
                                "Result" => {
                                    let mut args = args.args.iter();

                                    match (args.next(), args.next(), args.next(), false) {
                                        (Some(GenericArgument::Type(_)), _, None, true) => {
                                            // TODO: Wrap Result<T> alias to use int values if it is marked
                                            // to implement IntResult
                                        }
                                        (
                                            Some(GenericArgument::Type(a)),
                                            Some(GenericArgument::Type(b)),
                                            None,
                                            _,
                                        ) => {
                                            ret = Some((
                                                quote!(let #name = #name.into();),
                                                quote!(#name,),
                                                quote!(#name: #crate_path::result::CResult<#a, #b>,),
                                                quote!(#name: #crate_path::result::CResult<#a, #b>,),
                                                quote!(#name.into(),),
                                                false,
                                            ));
                                        }
                                        _ => {}
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }

                ret.unwrap_or_else(|| {
                    (
                        quote!(let #name = #name;),
                        quote!(#name,),
                        quote!(#name: #ty,),
                        quote!(#name: #ty,),
                        quote!(#name,),
                        true,
                    )
                })
            }
        };

        Self {
            arg,
            to_c_args,
            call_c_args,
            c_args,
            c_cast_args,
            to_trait_arg,
            trivial,
        }
    }
}

pub struct ParsedFunc {
    name: Ident,
    trait_name: Ident,
    safe: bool,
    abi: FuncAbi,
    has_nontrivial: bool,
    receiver: Receiver,
    args: Vec<TraitArg>,
    out: ParsedReturnType,
    generics: ParsedGenerics,
    sig_generics: ParsedGenerics,
}

impl ParsedFunc {
    pub fn new(
        sig: Signature,
        trait_name: Ident,
        generics: &ParsedGenerics,
        wrap_types: &BTreeMap<Ident, WrappedType>,
        res_override: Option<&Ident>,
        int_result: bool,
        crate_path: &TokenStream,
    ) -> Option<Self> {
        let name = sig.ident;
        let safe = sig.unsafety.is_none();
        let abi = From::from(sig.abi);
        let mut args: Vec<TraitArg> = vec![];

        let mut receiver = None;
        let mut has_nontrivial = false;

        let unsafety = if safe { quote!(unsafe) } else { quote!() };

        for input in sig.inputs.iter() {
            if let FnArg::Receiver(r) = &input {
                receiver = Some(r.clone());
            }
        }

        let receiver = receiver?;

        let out = ParsedReturnType::new(
            sig.output,
            wrap_types,
            res_override,
            int_result,
            &unsafety,
            (&name, &receiver),
            (crate_path, &trait_name, generics),
        );

        for input in sig.inputs.into_iter() {
            let func = TraitArg::new(
                input,
                wrap_types,
                crate_path,
                out.lifetime.as_ref(),
                out.lifetime_cast.as_ref(),
            );

            has_nontrivial = has_nontrivial || !func.trivial;

            args.push(func);
        }

        let generics = generics.clone();

        let sig_generics = ParsedGenerics::from(&sig.generics);

        Some(Self {
            name,
            trait_name,
            safe,
            abi,
            has_nontrivial,
            receiver,
            args,
            out,
            generics,
            sig_generics,
        })
    }

    pub fn ret_tmp_def(&self, stream: &mut TokenStream) {
        let name = &self.name;
        // If injected_ret_tmp exists, try using the static one, but skip it if it doesn't exist.
        if let (Some(_), Some(ty)) = (
            &self.out.injected_ret_tmp,
            self.out
                .injected_ret_tmp_static
                .as_ref()
                .or_else(|| self.out.injected_ret_tmp.as_ref()),
        ) {
            let gen = if self.receiver.mutability.is_some() {
                quote!(#name: ::core::mem::MaybeUninit<#ty>,)
            } else {
                quote!(#name: ::core::cell::Cell<::core::mem::MaybeUninit<#ty>>,)
            };
            stream.extend(gen);
        }
    }

    pub fn ret_default_def(&self, stream: &mut TokenStream) {
        let name = &self.name;
        if self.out.injected_ret_tmp.is_some() {
            let gen = if self.receiver.mutability.is_some() {
                quote!(#name: ::core::mem::MaybeUninit::uninit(),)
            } else {
                quote!(#name: ::core::cell::Cell::new(::core::mem::MaybeUninit::uninit()),)
            };
            stream.extend(gen);
        }
    }

    pub fn ret_getter_def(&self, stream: &mut TokenStream) {
        let name = &self.name;

        if let Some(ty) = &self.out.injected_ret_tmp {
            let gen = match (&self.out.lifetime, &self.receiver.mutability) {
                (Some(lt), Some(_)) => {
                    quote! {
                        fn #name<#lt>(&#lt mut self) -> &#lt mut ::core::mem::MaybeUninit<#ty> {
                            // SAFETY:
                            // We shorten the lifetime of the borrowed data.
                            // The data is not being dropped, and is not accessible
                            // from anywhere else, so this is safe.:w
                            unsafe { std::mem::transmute(&mut self.#name) }
                        }
                    }
                }
                (None, Some(_)) => {
                    quote! {
                        fn #name(&mut self) -> &mut ::core::mem::MaybeUninit<#ty> {
                            &mut self.#name
                        }
                    }
                }
                (Some(lt), None) => {
                    quote! {
                        #[allow(clippy::mut_from_ref)]
                        fn #name<#lt>(&#lt self) -> &#lt mut ::core::mem::MaybeUninit<#ty> {
                            // SAFETY:
                            // We mutably alias the underlying cell, which is not very safe, because
                            // it could already be borrowed immutably. However, for this particular case
                            // it is somewhat okay, with emphasis on "somewhat". If this function returns
                            // a constant, this method is safe, because the stack will be overriden with
                            // the exact same data.
                            //
                            // We shorten the lifetime of the borrowed data.
                            // The data is not being dropped, and is not accessible
                            // from anywhere else, so this is safe.:w
                            unsafe { std::mem::transmute(self.#name.as_ptr().as_mut().unwrap()) }
                        }
                    }
                }
                (None, None) => {
                    quote! {
                        #[allow(clippy::mut_from_ref)]
                        fn #name(&self) -> &mut ::core::mem::MaybeUninit<#ty> {
                            // SAFETY:
                            // We mutably alias the underlying cell, which is not very safe, because
                            // it could already be borrowed immutably. However, for this particular case
                            // it is somewhat okay, with emphasis on "somewhat". If this function returns
                            // a constant, this method is safe, because the stack will be overriden with
                            // the exact same data.
                            unsafe { self.#name.as_ptr().as_mut().unwrap() }
                        }
                    }
                }
            };

            stream.extend(gen);
        }
    }

    pub fn vtbl_args(&self) -> TokenStream {
        let mut ret = TokenStream::new();

        for arg in &self.args {
            arg.c_args.to_tokens(&mut ret);
        }

        ret
    }

    pub fn vtbl_args_cast(&self) -> TokenStream {
        let mut ret = TokenStream::new();

        for arg in &self.args {
            arg.c_cast_args.to_tokens(&mut ret);
        }

        ret
    }

    pub fn trait_args(&self) -> TokenStream {
        let mut ret = TokenStream::new();

        for arg in &self.args {
            let arg = &arg.arg;
            let arg = quote!(#arg, );
            arg.to_tokens(&mut ret);
        }

        ret
    }

    pub fn trait_passthrough_args(&self, skip: usize) -> TokenStream {
        let mut ret = TokenStream::new();

        for arg in self.args.iter().skip(skip) {
            match &arg.arg {
                FnArg::Typed(ty) => {
                    let pat = &ty.pat;
                    quote!(#pat,).to_tokens(&mut ret);
                }
                FnArg::Receiver(_) => {
                    quote!(self,).to_tokens(&mut ret);
                }
            }
        }

        ret
    }

    pub fn to_c_def_args(&self) -> TokenStream {
        let mut ret = TokenStream::new();

        for arg in &self.args {
            arg.to_c_args.to_tokens(&mut ret);
        }

        ret
    }

    pub fn to_c_call_args(&self) -> TokenStream {
        let mut ret = TokenStream::new();

        for arg in &self.args {
            arg.call_c_args.to_tokens(&mut ret);
        }

        ret
    }

    pub fn to_trait_call_args(&self) -> TokenStream {
        let mut ret = TokenStream::new();

        for arg in &self.args {
            arg.to_trait_arg.to_tokens(&mut ret);
        }

        ret
    }

    /// Create a VTable definition for this function
    pub fn vtbl_def(&self, stream: &mut TokenStream) {
        let name = &self.name;
        let args = self.vtbl_args();
        let ParsedReturnType {
            c_out,
            c_cast_out,
            c_ret_params,
            lifetime,
            lifetime_cast,
            unbounded_hrtb,
            ..
        } = &self.out;

        let ParsedGenerics {
            life_declare: sig_life_declare,
            ..
        } = &self.sig_generics;

        let (hrtb, args, c_out) = match (
            lifetime.as_ref().filter(|lt| lt.ident != "cglue_a"),
            lifetime_cast,
            *unbounded_hrtb,
        ) {
            (_, Some(lifetime), false) => (quote!(#lifetime), self.vtbl_args_cast(), c_cast_out),
            (Some(lifetime), _, _) => (quote!(#lifetime), args, c_out),
            _ => (quote!(), args, c_out),
        };

        let gen = quote! {
            #name: for<#sig_life_declare #hrtb> extern "C" fn(#args #c_ret_params) #c_out,
        };

        stream.extend(gen);
    }

    /// Create a VTable definition for this function
    pub fn vtbl_getter_def(&self, stream: &mut TokenStream) {
        let name = &self.name;
        let args = self.vtbl_args();
        let ParsedReturnType {
            c_out,
            c_cast_out,
            c_ret_params,
            lifetime,
            lifetime_cast,
            unbounded_hrtb,
            ..
        } = &self.out;

        let ParsedGenerics {
            life_declare: sig_life_declare,
            ..
        } = &self.sig_generics;

        let (hrtb, args, c_out) = match (
            lifetime.as_ref().filter(|lt| lt.ident != "cglue_a"),
            lifetime_cast,
            *unbounded_hrtb,
        ) {
            (_, Some(lifetime), false) => (quote!(#lifetime), self.vtbl_args_cast(), c_cast_out),
            (Some(lifetime), _, _) => (quote!(#lifetime), args, c_out),
            _ => (quote!(), args, c_out),
        };

        let doc_text = format!(" Getter for {}.", name.to_string());

        let gen = quote! {
            #[doc = #doc_text]
            ///
            /// Note that this function is wrapped into unsafe, because if already were is an
            /// opaque one, it would allow to invoke undefined behaviour.
            pub fn #name(&self) -> for<#sig_life_declare #hrtb> unsafe extern "C" fn(#args #c_ret_params) #c_out {
                unsafe { ::core::mem::transmute(self.#name) }
            }
        };

        stream.extend(gen);

        if lifetime_cast.is_some() && *unbounded_hrtb {
            let name2 = format_ident!("{}_lifetimed", name);

            let safety = self.get_safety();

            let args_cast = self.vtbl_args_cast();

            let gen = quote! {
                #[doc = #doc_text]
                ///
                /// This function has its argument lifetime cast so that it's usable with anonymous
                /// lifetime functions.
                ///
                /// # Safety
                ///
                /// This ought to only be used when references to objects are being returned,
                /// otherwise there is a risk of lifetime rule breakage.
                unsafe fn #name2(&self) -> for<#lifetime_cast> #safety extern "C" fn(#args_cast #c_ret_params) #c_cast_out {
                    ::core::mem::transmute(self.#name)
                }
            };

            stream.extend(gen);
        }
    }

    pub fn is_wrapped(&self) -> bool {
        self.abi == FuncAbi::Wrapped || self.has_nontrivial
    }

    /// Create a wrapper implementation body for this function
    ///
    /// If the function is ReprC already, it will not be wrapped and will return `None`
    pub fn cfunc_def(
        &self,
        tokens: &mut TokenStream,
        trg_path: &TokenStream,
    ) -> (Option<&TokenStream>, bool) {
        if !self.is_wrapped() {
            return (None, false);
        }

        let name = &self.name;
        let args = self.vtbl_args();
        let ParsedReturnType {
            c_out,
            c_where_bounds,
            c_where_bounds_cast,
            c_ret,
            c_pre_call,
            c_ret_params,
            use_hrtb,
            return_self,
            lifetime,
            lifetime_cast,
            unbounded_hrtb,
            needs_ctx,
            ..
        } = &self.out;
        let call_args = self.to_trait_call_args();

        let trname = &self.trait_name;
        let fnname = format_ident!("{}{}", FN_PREFIX, name);
        let safety = self.get_safety();

        let ParsedGenerics {
            life_declare,
            life_use,
            gen_declare,
            gen_use,
            gen_where_bounds,
            ..
        } = &self.generics;

        let ParsedGenerics {
            life_declare: sig_life_declare,
            ..
        } = &self.sig_generics;

        let tmp_lifetime = if *use_hrtb && !life_use.is_empty() {
            quote!('cglue_b, )
        } else {
            quote!(#life_use)
        };

        // Inject 'cglue_a if there are no lifetimes declared by the trait,
        // and temp lifetime is needed
        let life_declare = if lifetime.is_some() && life_declare.is_empty() {
            quote!(#lifetime, )
        } else {
            life_declare.to_token_stream()
        };

        let mut container_bound = TokenStream::new();

        let (c_pre_call, this) = if self.receiver.reference.is_none() {
            container_bound.extend(quote!(#trg_path::IntoInner<InnerTarget = CGlueF> + ));
            if *needs_ctx {
                (
                    quote!(
                        let (thisobj, cglue_ctx) = unsafe { thisobj.split_ctx_owned() };
                        #c_pre_call
                    ),
                    quote!(thisobj),
                )
            } else {
                (
                    quote!(
                        let thisobj = unsafe { thisobj.into_inner() };
                        #c_pre_call
                    ),
                    quote!(thisobj),
                )
            }
        } else {
            (quote!(#c_pre_call), quote!(thisptr))
        };

        let cglue_c = if *needs_ctx {
            container_bound
                .extend(quote!(#trg_path::ContextRef<Context = CGlueC, ObjType = CGlueF> + ));

            if self.receiver.reference.is_some() {
                container_bound.extend(quote!(::core::ops::Deref<Target = CGlueF>+));

                if self.receiver.mutability.is_some() {
                    container_bound.extend(quote!(::core::ops::DerefMut+));
                    container_bound.extend(quote!(#trg_path::ContextMut + ));
                }
            } else {
                container_bound.extend(quote!(#trg_path::ContextMut + ));
                container_bound.extend(quote!(#trg_path::ContextOwned + ));
            }

            Some(
                quote!(CGlueC: 'static + Clone + #trg_path::Opaquable, CGlueD: 'static + Clone + #trg_path::Opaquable, ),
            )
        } else {
            None
        };

        let container_bound = if !container_bound.is_empty() {
            quote!(CGlueT: #container_bound,)
        } else if *return_self {
            quote!(CGlueT,)
        } else {
            container_bound
        };

        let c_where_bounds = if lifetime_cast.is_some() && *unbounded_hrtb {
            c_where_bounds_cast
        } else {
            c_where_bounds
        };

        let gen = quote! {
            #safety extern "C" fn #fnname<#sig_life_declare #life_declare #container_bound #cglue_c CGlueF: for<'cglue_b> #trname<#tmp_lifetime #gen_use>, #gen_declare>(#args #c_ret_params) #c_out where #gen_where_bounds #c_where_bounds {
                #c_pre_call
                let ret = #this.#name(#call_args);
                #c_ret
            }
        };

        tokens.extend(gen);

        (Some(c_where_bounds), *needs_ctx)
    }

    pub fn vtbl_default_def(&self, tokens: &mut TokenStream) {
        let name = &self.name;

        let fnname: TokenStream = if self.is_wrapped() {
            let generics = if self.out.needs_ctx {
                let gen_use = &self.generics.gen_use;

                quote!(::<CGlueT, CGlueC, CGlueD, CGlueF, #gen_use>)
            } else {
                quote!()
            };

            format!("{}{}{}", FN_PREFIX, name, generics)
        } else {
            format!("CGlueF::{}", name)
        }
        .parse()
        .unwrap();

        tokens.extend(quote!(#name: #fnname,));
    }

    pub fn get_safety(&self) -> TokenStream {
        if self.safe {
            quote!()
        } else {
            quote!(unsafe)
        }
    }

    pub fn trait_impl(&self, tokens: &mut TokenStream) -> (bool, bool, bool) {
        let name = &self.name;
        let args = self.trait_args();
        let ParsedReturnType {
            ty: out,
            impl_func_ret,
            c_ret_precall_def,
            c_call_ret_args,
            lifetime_cast,
            unbounded_hrtb,
            ..
        } = &self.out;
        let def_args = self.to_c_def_args();
        let call_args = self.to_c_call_args();
        let safety = self.get_safety();
        let abi = self.abi.prefix();

        let ParsedGenerics {
            life_declare: sig_life_declare,
            ..
        } = &self.sig_generics;

        let get_vfunc = if lifetime_cast.is_some() && *unbounded_hrtb {
            let name_lifetimed = format_ident!("{}_lifetimed", name);
            quote!(unsafe { self.get_vtbl().#name_lifetimed() })
        } else {
            quote!(self.get_vtbl().#name)
        };

        let gen = quote! {
            #[inline(always)]
            #safety #abi fn #name <#sig_life_declare> (#args) #out {
                let __cglue_vfunc = #get_vfunc;
                #def_args
                #c_ret_precall_def
                let mut ret = __cglue_vfunc(#call_args #c_call_ret_args);
                #impl_func_ret
            }
        };

        tokens.extend(gen);

        (
            self.receiver.mutability.is_some(),
            self.receiver.reference.is_none(),
            self.out.return_self,
        )
    }

    pub fn forward_wrapped_trait_impl(&self, tokens: &mut TokenStream) -> bool {
        if self.receiver.reference.is_none() {
            return false;
        }

        let name = &self.name;
        let args = self.trait_args();
        let passthrough_args = self.trait_passthrough_args(1);
        let ParsedReturnType {
            ty: out, use_wrap, ..
        } = &self.out;
        let safety = self.get_safety();
        let abi = self.abi.prefix();

        let ParsedGenerics {
            life_declare,
            gen_declare,
            ..
        } = &self.sig_generics;

        let return_out = if *use_wrap {
            quote!(Self(ret))
        } else {
            quote!(ret)
        };

        let gen = quote! {
            #[inline(always)]
            #safety #abi fn #name <#life_declare #gen_declare> (#args) #out {
                let ret = (self.0).#name(#passthrough_args);
                #return_out
            }
        };

        tokens.extend(gen);

        self.receiver.mutability.is_some()
    }

    pub fn arc_wrapped_trait_impl(&self, tokens: &mut TokenStream) {
        let name = &self.name;
        let args = self.trait_args();
        let passthrough_args = self.trait_passthrough_args(1);
        let ParsedReturnType {
            ty: out, use_wrap, ..
        } = &self.out;
        let safety = self.get_safety();
        let abi = self.abi.prefix();

        let ParsedGenerics {
            life_declare,
            gen_declare,
            ..
        } = &self.sig_generics;

        let get_inner = if self.receiver.reference.is_none() {
            quote!(self.into_inner())
        } else if self.receiver.mutability.is_some() {
            quote!(self.as_mut())
        } else {
            quote!(self.as_ref())
        };

        let return_out = if *use_wrap {
            quote!((ret, arc).into())
        } else {
            quote!(ret)
        };

        let gen = quote! {
            #[inline(always)]
            #safety #abi fn #name <#life_declare #gen_declare> (#args) #out {
                let (inner, arc) = #get_inner;
                let ret = inner.#name(#passthrough_args);
                #return_out
            }
        };

        tokens.extend(gen);
    }

    pub fn int_trait_impl(
        &self,
        ext_path: Option<&Path>,
        ext_name: &Ident,
        tokens: &mut TokenStream,
    ) {
        let name = &self.name;
        let args = self.trait_args();
        let passthrough_args = self.trait_passthrough_args(0);
        let ParsedReturnType { ty: out, .. } = &self.out;
        let safety = self.get_safety();
        let abi = self.abi.prefix();

        let gen = quote! {
            #[inline(always)]
            #safety #abi fn #name (#args) #out {
                #ext_path #ext_name::#name(#passthrough_args)
            }
        };

        tokens.extend(gen);
    }
}

#[derive(Debug, Eq, PartialEq)]
enum FuncAbi {
    ReprC,
    Wrapped,
}

impl FuncAbi {
    pub fn prefix(&self) -> TokenStream {
        match self {
            FuncAbi::ReprC => quote!(extern "C"),
            FuncAbi::Wrapped => quote!(),
        }
    }
}

impl From<Option<Abi>> for FuncAbi {
    fn from(abi: Option<Abi>) -> Self {
        // If the function uses C abi, we can omit wrapper functions
        if let Some(Abi {
            extern_token: _,
            name: Some(abi),
        }) = abi
        {
            if abi.value() == "C" {
                return FuncAbi::ReprC;
            }
        }

        FuncAbi::Wrapped
    }
}

struct ParsedReturnType {
    ty: ReturnType,
    c_out: TokenStream,
    c_cast_out: TokenStream,
    c_where_bounds: TokenStream,
    c_where_bounds_cast: TokenStream,
    c_ret_params: TokenStream,
    c_ret_precall_def: TokenStream,
    c_call_ret_args: TokenStream,
    c_pre_call: Option<TokenStream>,
    c_ret: TokenStream,
    impl_func_ret: TokenStream,
    /// Whether HRTB and tmp stack should be injected.
    ///
    /// HRTB is the `for<'cglue_b>` bound to bind `this` lifetime to be the same one as another
    /// argument's, as well as the return type's. This is only relevant when tmp_ret is being
    /// used. In addition to that, generic bounds will be added to the C wrapper for equivalency.
    injected_ret_tmp: Option<GenericType>,
    injected_ret_tmp_static: Option<GenericType>,
    use_hrtb: bool,
    lifetime: Option<Lifetime>,
    lifetime_cast: Option<Lifetime>,
    unbounded_hrtb: bool,
    return_self: bool,
    use_wrap: bool,
    needs_ctx: bool,
}

// TODO: handle more cases
#[allow(clippy::single_match)]
fn wrapped_lifetime(mut ty: Type, target: Lifetime) -> Type {
    match &mut ty {
        Type::Path(path) => {
            for seg in path.path.segments.iter_mut() {
                if let PathArguments::AngleBracketed(args) = &mut seg.arguments {
                    for arg in args.args.iter_mut() {
                        match arg {
                            GenericArgument::Lifetime(lt) => {
                                if lt.ident != format_ident!("static") {
                                    *lt = target.clone()
                                }
                            }
                            GenericArgument::Type(ty) => {
                                *ty = wrapped_lifetime(ty.clone(), target.clone())
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        _ => {}
    }
    ty
}

impl ParsedReturnType {
    #[allow(clippy::never_loop)]
    fn new(
        mut ty: ReturnType,
        targets: &BTreeMap<Ident, WrappedType>,
        res_override: Option<&Ident>,
        int_result: bool,
        unsafety: &TokenStream,
        (func_name, receiver): (&Ident, &Receiver),
        (crate_path, trait_name, trait_generics): (&TokenStream, &Ident, &ParsedGenerics),
    ) -> Self {
        let mut ret = Self {
            ty: ty.clone(),
            c_out: ty.to_token_stream(),
            c_cast_out: ty.to_token_stream(),
            c_where_bounds: quote!(),
            c_where_bounds_cast: quote!(),
            c_ret_params: quote!(),
            c_ret_precall_def: quote!(),
            c_call_ret_args: quote!(),
            c_pre_call: None,
            c_ret: quote!(ret),
            impl_func_ret: quote!(ret),
            injected_ret_tmp: None,
            injected_ret_tmp_static: None,
            use_hrtb: false,
            lifetime: None,
            lifetime_cast: None,
            unbounded_hrtb: false,
            return_self: false,
            use_wrap: false,
            needs_ctx: false,
        };

        if let ReturnType::Type(_, ty) = &mut ty {
            let mut ty_cast = None;

            if let Some(wrapped) = wrap_type(&mut *ty, targets) {
                let old_ty = wrapped.0;
                let trait_ty = wrapped.1;
                let WrappedType {
                    return_conv,
                    lifetime_bound,
                    lifetime_type_bound,
                    other_bounds,
                    other_bounds_simple,
                    inject_ret_tmp,
                    impl_return_conv,
                    ty: new_ty,
                    ty_static,
                    needs_ctx,
                    ..
                } = wrapped.2;

                // TODO: sort out the order

                let (mutable, lifetime) = match (inject_ret_tmp, &**ty) {
                    (true, Type::Reference(ty)) => {
                        (ty.mutability.is_some(), ty.lifetime.as_ref().cloned())
                    }
                    (false, _) => (false, None),
                    _ => panic!("Wrapped ref return currently only valid for references!"),
                };

                let unbounded_hrtb = lifetime.is_none() && lifetime_type_bound.is_none();

                let cglue_b_lifetime = Lifetime {
                    apostrophe: proc_macro2::Span::call_site(),
                    ident: format_ident!("cglue_b"),
                };

                let lifetime = lifetime.or_else(|| lifetime_bound.clone()).or_else(|| {
                    Some(Lifetime {
                        apostrophe: proc_macro2::Span::call_site(),
                        ident: format_ident!("cglue_a"),
                    })
                });

                let lifetime_cast = if lifetime
                    .as_ref()
                    .filter(|lt| lt.ident == "cglue_a")
                    .is_some()
                {
                    ty_cast = Some(ty.clone());
                    Some(cglue_b_lifetime.clone())
                } else {
                    None
                };

                if let Some(lifetime) = &lifetime {
                    **ty = wrapped_lifetime(*ty.clone(), lifetime.clone());
                }

                // TODO: should this inherit lifetime, or just fallback on lifetime?
                let lifetime_type_bound = lifetime_type_bound.clone().map(|lt| {
                    if lt.ident != "static" {
                        cglue_b_lifetime
                    } else {
                        lt
                    }
                });

                let ret_wrap = match return_conv {
                    Some(conv) => quote! {
                        let mut conv = #conv;
                        conv(ret)
                    },
                    _ => quote!(ret.into()),
                };

                let life_use = &trait_generics.life_use;
                let gen_use = &trait_generics.gen_use;

                // TODO: where do we need this bound?

                let is_static = lifetime_type_bound.map(|l| l.ident == "static") == Some(true);

                let (static_bound, static_bound_simple) = if is_static {
                    if life_use.is_empty() {
                        (
                            quote!(for<'cglue_b> <CGlueF as #trait_name<#gen_use>>::#trait_ty: 'static,),
                            quote!(for<'cglue_b> <CGlueF as #trait_name<#gen_use>>::#trait_ty: 'static,),
                        )
                    } else {
                        (
                            quote!(for<'cglue_b> <CGlueF as #trait_name<'cglue_b, #gen_use>>::#trait_ty: 'static,),
                            quote!(<CGlueF as #trait_name<'cglue_a, #gen_use>>::#trait_ty: 'static,),
                        )
                    }
                } else {
                    (quote!(), quote!())
                };

                let where_bound = quote!(#static_bound #other_bounds);
                let where_bound_simple = quote!(#static_bound_simple #other_bounds_simple);

                // Replace the lifetime of the type.
                if let (true, Type::Reference(ty)) = (inject_ret_tmp, &mut **ty) {
                    ty.lifetime = lifetime.clone();

                    if let Some(ty_cast) = &mut ty_cast {
                        if let Type::Reference(ty_cast) = &mut **ty_cast {
                            ty_cast.lifetime = lifetime_cast.clone();
                        }
                    }
                };

                let (injected_ret_tmp, tmp_type_def, tmp_impl_def, tmp_call_def) =
                    match (inject_ret_tmp, mutable) {
                        (true, false) => (
                            Some(new_ty.clone()),
                            quote!(ret_tmp: &mut ::core::mem::MaybeUninit<#new_ty>,),
                            quote!(let ret_tmp = ret_tmp.#func_name();),
                            quote!(ret_tmp,),
                        ),
                        (true, true) => (
                            Some(new_ty.clone()),
                            quote!(ret_tmp: &mut ::core::mem::MaybeUninit<#new_ty>,),
                            quote!(let ret_tmp = ret_tmp.#func_name();),
                            quote!(ret_tmp,),
                        ),
                        _ => (None, quote!(), quote!(), quote!()),
                    };

                let (tmp_type_def, c_pre_call, tmp_call_def) =
                    if *needs_ctx && receiver.reference.is_some() {
                        (
                            quote!(#tmp_type_def cglue_ctx: &CGlueC,),
                            Some(quote!(#tmp_impl_def let cglue_ctx = cglue_ctx.clone();)),
                            quote!(#tmp_call_def cglue_ctx,),
                        )
                    } else {
                        (tmp_type_def, None, tmp_call_def)
                    };

                let impl_return_conv = impl_return_conv
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| quote!(ret));

                let return_self = trait_ty == "Self";

                ret.c_out = quote!(-> #ty);
                ret.c_cast_out = quote!(-> #ty_cast);

                // If we are returning self, do not actually change the return type.
                // I know, messy :(
                if return_self {
                    *ty = Box::new(old_ty);
                }

                ret.c_where_bounds = where_bound;
                ret.c_where_bounds_cast = where_bound_simple;
                ret.c_ret_params = tmp_type_def;
                ret.c_ret_precall_def = tmp_impl_def;
                ret.c_call_ret_args = tmp_call_def;
                ret.c_pre_call = c_pre_call;
                ret.c_ret = quote!(#ret_wrap);
                ret.impl_func_ret = impl_return_conv;
                ret.injected_ret_tmp = injected_ret_tmp;
                ret.injected_ret_tmp_static = ty_static.clone();
                ret.use_hrtb = true;
                ret.unbounded_hrtb = unbounded_hrtb;
                ret.return_self = return_self;
                ret.use_wrap = true;
                ret.lifetime = lifetime;
                ret.lifetime_cast = lifetime_cast;
                ret.needs_ctx = *needs_ctx;
            }

            if let Type::Path(p) = &mut **ty {
                let last = p.path.segments.last();
                if let Some((PathArguments::AngleBracketed(args), last)) =
                    last.map(|l| (&l.arguments, l))
                {
                    let ident = &last.ident;

                    let result_ident = format_ident!("Result");

                    let result_ident = res_override.unwrap_or(&result_ident);

                    match ident.to_string().as_str() {
                        "Option" => {
                            if let Some(GenericArgument::Type(a)) = args.args.first() {
                                if !crate::util::is_null_pointer_optimizable(a, &[]) {
                                    let new_path: Path =
                                        parse2(quote!(#crate_path::option::COption))
                                            .expect("Failed to parse COption path");

                                    replace_path_keep_final_args(Some(&mut **ty), new_path.clone());
                                    replace_path_keep_final_args(ty_cast.as_deref_mut(), new_path);

                                    ret.c_out = quote!(-> #ty);
                                    ret.c_cast_out = quote!(-> #ty_cast);
                                    ret.c_ret.extend(quote!(.into()));
                                    ret.impl_func_ret.extend(quote!(.into()));
                                }
                            }
                        }
                        _ => {
                            if ident == result_ident {
                                let mut args = args.args.iter();

                                let to_match = (args.next(), args.next(), args.next(), int_result);

                                std::mem::drop(args);

                                match to_match {
                                    (Some(GenericArgument::Type(a)), _, None, true) => loop {
                                        ret.c_out = quote!(-> i32);
                                        ret.c_cast_out = quote!(-> i32);

                                        let c_ret = &ret.c_ret;

                                        let mapped_ret = quote! {
                                            let ret = ret.map(|ret| {
                                                #c_ret
                                            });
                                        };

                                        if let Type::Tuple(tup) = a {
                                            if tup.elems.is_empty() {
                                                ret.c_ret = quote! {
                                                    #mapped_ret
                                                    #crate_path::result::into_int_result(ret)
                                                };
                                                let impl_func_ret = &ret.impl_func_ret;
                                                ret.impl_func_ret = quote!(#crate_path::result::from_int_result_empty(#impl_func_ret));

                                                break;
                                            }
                                        }

                                        ret.c_ret_params.extend(
                                            quote!(ok_out: &mut ::core::mem::MaybeUninit<#a>,),
                                        );
                                        ret.c_ret_precall_def.extend(quote!(let mut ok_out = ::core::mem::MaybeUninit::uninit();));
                                        ret.c_call_ret_args.extend(quote!(&mut ok_out,));

                                        ret.c_ret = quote! {
                                            #mapped_ret
                                            #crate_path::result::into_int_out_result(ret, ok_out)
                                        };
                                        let impl_func_ret = &ret.impl_func_ret;
                                        ret.impl_func_ret = quote!(#unsafety { #crate_path::result::from_int_result(#impl_func_ret, ok_out) });

                                        break;
                                    },
                                    (
                                        Some(GenericArgument::Type(_)),
                                        Some(GenericArgument::Type(_)),
                                        None,
                                        _,
                                    ) => {
                                        let new_path: Path =
                                            parse2(quote!(#crate_path::result::CResult))
                                                .expect("Failed to parse CResult path");

                                        replace_path_keep_final_args(
                                            Some(&mut **ty),
                                            new_path.clone(),
                                        );
                                        replace_path_keep_final_args(
                                            ty_cast.as_deref_mut(),
                                            new_path,
                                        );

                                        ret.c_out = quote!(-> #ty);
                                        ret.c_cast_out = quote!(-> #ty_cast);

                                        let c_ret = &ret.c_ret;

                                        let mapped_ret = quote! {
                                            let ret = ret.map(|ret| {
                                                #c_ret
                                            });
                                        };

                                        ret.c_ret = quote! {
                                            #mapped_ret
                                            ret.into()
                                        };

                                        ret.impl_func_ret.extend(quote!(.into()));
                                    }
                                    _ => {}
                                };
                            }
                        }
                    }
                }
            }
        }

        ret
    }
}

fn replace_path_keep_final_args(ty: Option<&mut Type>, new_path: Path) {
    if let Some(Type::Path(path)) = ty {
        let old_path = std::mem::replace(&mut path.path, new_path);
        if let Some(seg) = old_path.segments.into_iter().last() {
            if let Some(new_seg) = path.path.segments.iter_mut().last() {
                new_seg.arguments = seg.arguments;
            }
        }
    }
}
