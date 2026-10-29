//! Compile-time CDEF scaffold. Wire validation and decoding remain in mac-definition.
use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, GenericArgument, ItemFn, PathArguments, Type, parse_macro_input, spanned::Spanned};

/// Generate the native CDEF entry and panic scaffold for a safe `fn main`.
#[proc_macro_attribute]
pub fn cdef_main(arguments: TokenStream, input: TokenStream) -> TokenStream
{
    if !arguments.is_empty()
    {
        return syn::Error::new(proc_macro::Span::call_site().into(),
                               "cdef_main takes no arguments; the Message state selects its \
                                family").to_compile_error()
                                        .into();
    }
    let function = parse_macro_input!(input as ItemFn);
    match expand(function)
    {
        Ok(expansion) => expansion.into(),
        Err(error) => error.to_compile_error().into()
    }
}

fn expand(function: ItemFn) -> syn::Result<proc_macro2::TokenStream>
{
    let signature = &function.sig;
    if signature.ident != "main"
       || signature.unsafety.is_some()
       || signature.asyncness.is_some()
       || signature.constness.is_some()
       || signature.abi.is_some()
       || signature.variadic.is_some()
       || signature.inputs.len() != 2
       || signature.generics
                   .params
                   .iter()
                   .any(|parameter| !matches!(parameter, syn::GenericParam::Lifetime(_)))
    {
        return Err(syn::Error::new(signature.span(),
                                   "cdef_main requires a safe fn main(Message<'a, State>, \
                                    Services<'a>) -> Result<Reply>"));
    }
    let Some(FnArg::Typed(message)) = signature.inputs.first()
    else
    {
        return Err(syn::Error::new(signature.span(), "cdef_main requires a Message argument"));
    };
    let Type::Path(message_type) = message.ty.as_ref()
    else
    {
        return Err(syn::Error::new(message.span(), "expected Message<'a, State>"));
    };
    let segment =
        message_type.path
                    .segments
                    .last()
                    .ok_or_else(|| {
                        syn::Error::new(message_type.span(), "expected Message<'a, State>")
                    })?;
    let PathArguments::AngleBracketed(arguments) = &segment.arguments
    else
    {
        return Err(syn::Error::new(message_type.span(), "expected Message<'a, State>"));
    };
    if segment.ident != "Message" || message_type.qself.is_some()
    {
        return Err(syn::Error::new(message_type.span(), "expected Message<'a, State>"));
    }
    let mut types = arguments.args.iter().filter_map(|argument| {
                                             if let GenericArgument::Type(state) = argument
                                             {
                                                 Some(state)
                                             }
                                             else
                                             {
                                                 None
                                             }
                                         });
    let state = types.next()
                     .ok_or_else(|| {
                         syn::Error::new(arguments.span(), "Message must name its CDEF state type")
                     })?;
    if types.next().is_some()
    {
        return Err(syn::Error::new(arguments.span(), "expected one Message state type"));
    }
    let binding = if let Type::Path(state_type) = state
                  {
                      state_type.path
                                .segments
                                .last()
                                .filter(|_| state_type.qself.is_none())
                                .and_then(|segment| {
                                    match segment.ident.to_string().as_str()
                                    {
                                        "Button" => Some(quote!(buttons_entry)),
                                        "Scrollbar" => Some(quote!(scrollbar_entry)),
                                        "Progress" => Some(quote!(progress_entry)),
                                        "StaticText" => Some(quote!(static_text_entry)),
                                        _ => None
                                    }
                                })
                  }
                  else
                  {
                      None
                  }.ok_or_else(|| {
                       syn::Error::new(state.span(),
                                       format!("unsupported CDEF state type `{}`; expected \
                                                Button, Scrollbar, Progress or StaticText",
                                               quote!(#state)))
                   })?;
    Ok(quote! {
        #function

        #[cfg(target_arch = "riscv64")]
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".text.entry")]
        pub unsafe extern "C" fn _start(call: *mut ::mac_definition::__abi::DefinitionCall) -> i64
        {
            unsafe { ::mac_definition::#binding(call, main) }
        }

        #[cfg(target_arch = "riscv64")]
        #[panic_handler]
        fn __definition_panic(info: &::core::panic::PanicInfo<'_>) -> !
        {
            ::mac_definition::runtime::panic(info)
        }
    })
}
