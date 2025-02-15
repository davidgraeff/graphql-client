use crate::deprecation::DeprecationStatus;
use crate::introspection_response;
use crate::objects::GqlObjectField;
use crate::query::QueryContext;
use crate::schema::Schema;
use failure;
use graphql_parser;
use heck::SnakeCase;
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use std::cell::Cell;
use std::collections::HashMap;

/// Represents an input object type from a GraphQL schema
#[derive(Debug, Clone, PartialEq)]
pub struct GqlInput<'schema> {
    pub description: Option<&'schema str>,
    pub name: &'schema str,
    pub fields: HashMap<&'schema str, GqlObjectField<'schema>>,
    pub is_required: Cell<bool>,
}

impl<'schema> GqlInput<'schema> {
    pub(crate) fn require(&self, schema: &Schema<'schema>) {
        if self.is_required.get() {
            return;
        }
        self.is_required.set(true);
        self.fields.values().for_each(|field| {
            schema.require(&field.type_.inner_name_str());
        })
    }

    fn contains_type_without_indirection(
        &self,
        context: &QueryContext<'_, '_>,
        type_name: &str,
    ) -> bool {
        // the input type is recursive if any of its members contains it, without indirection
        self.fields.values().any(|field| {
            // the field is indirected, so no boxing is needed
            if field.type_.is_indirected() {
                return false;
            }

            let field_type_name = field.type_.inner_name_str();
            let input = context.schema.inputs.get(field_type_name);

            if let Some(input) = input {
                // the input contains itself, not indirected
                if input.name == type_name {
                    return true;
                }

                // we check if the other input contains this one (without indirection)
                input.contains_type_without_indirection(context, type_name)
            } else {
                // the field is not referring to an input type
                false
            }
        })
    }

    fn is_recursive_without_indirection(&self, context: &QueryContext<'_, '_>) -> bool {
        self.contains_type_without_indirection(context, &self.name)
    }

    fn map_field(
        &self,
        context: &QueryContext<'_, '_>,
        field: &GqlObjectField<'_>,
        required_fields: &mut Vec<TokenStream>,
        struct_field_assignments: &mut Vec<TokenStream>,
    ) -> TokenStream {
        let is_recursive =
            if let Some(input) = context.schema.inputs.get(field.type_.inner_name_str()) {
                input.is_recursive_without_indirection(context)
            } else {
                false
            };

        // If the type is recursive, we have to box it
        let ty = if is_recursive {
            match &field.type_ {
                // If it's an optional field: Wrap the boxed inner type in an Option
                crate::field_type::FieldType::Optional(inner) => {
                    let ty = inner.to_rust(&context, "");
                    quote! { Option<Box<#ty>> }
                }
                _ => {
                    let ty = field.type_.to_rust(&context, "");
                    quote! { Box<#ty> }
                }
            }
        } else {
            let ty = field.type_.to_rust(&context, "");
            quote!(#ty)
        };

        context.schema.require(&field.type_.inner_name_str());
        let rust_safe_field_name = crate::shared::keyword_replace(&field.name.to_snake_case());
        let mut rename = crate::shared::field_rename_annotation(&field.name, &rust_safe_field_name);
        let name = Ident::new(&rust_safe_field_name, Span::call_site());

        match &field.type_ {
            crate::field_type::FieldType::Optional(_) => {
                struct_field_assignments.push(quote!(#name: None));
                rename = quote!(
                    #[serde(skip_serializing_if = "Option::is_none")]
                    #rename
                )
            }
            _ => {
                required_fields.push(quote!(#name: #ty));
                struct_field_assignments.push(quote!(#name: #name));
            }
        };
        quote!(#rename pub #name: #ty)
    }

    pub(crate) fn to_rust(
        &self,
        context: &QueryContext<'_, '_>,
    ) -> Result<TokenStream, failure::Error> {
        let mut obj_fields: Vec<&GqlObjectField<'_>> = self.fields.values().collect();
        obj_fields.sort_unstable_by(|a, b| a.name.cmp(&b.name));

        let mut fields: Vec<TokenStream> = vec![];
        let mut required_fields: Vec<TokenStream> = vec![];
        let mut struct_field_assignments: Vec<TokenStream> = vec![];

        for field in obj_fields.iter() {
            fields.push(self.map_field(
                context,
                field,
                &mut required_fields,
                &mut struct_field_assignments,
            ));
        }
        let variables_derives = context.variables_derives();

        // Prevent generated code like "pub struct crate" for a schema input like "input crate { ... }"
        // This works in tandem with renamed struct Variables field types, eg: pub struct Variables { pub criteria : crate_ , }
        let rust_safe_field_name = crate::shared::keyword_replace(&self.name);
        let name = Ident::new(&rust_safe_field_name, Span::call_site());

        Ok(quote! {
            #variables_derives
            pub struct #name {
                #(#fields,)*
            }
            impl #name {
                pub fn new(#(#required_fields),*) -> Self {
                    Self {
                        #(#struct_field_assignments,)*
                    }
                }
            }
        })
    }
}

impl<'schema> ::std::convert::From<&'schema graphql_parser::schema::InputObjectType>
    for GqlInput<'schema>
{
    fn from(schema_input: &'schema graphql_parser::schema::InputObjectType) -> GqlInput<'schema> {
        GqlInput {
            description: schema_input.description.as_ref().map(String::as_str),
            name: &schema_input.name,
            fields: schema_input
                .fields
                .iter()
                .map(|field| {
                    let name = field.name.as_str();
                    let field = GqlObjectField {
                        description: None,
                        name: &field.name,
                        type_: crate::field_type::FieldType::from(&field.value_type),
                        deprecation: DeprecationStatus::Current,
                    };
                    (name, field)
                })
                .collect(),
            is_required: false.into(),
        }
    }
}

impl<'schema> ::std::convert::From<&'schema introspection_response::FullType>
    for GqlInput<'schema>
{
    fn from(schema_input: &'schema introspection_response::FullType) -> GqlInput<'schema> {
        GqlInput {
            description: schema_input.description.as_ref().map(String::as_str),
            name: schema_input
                .name
                .as_ref()
                .map(String::as_str)
                .expect("unnamed input object"),
            fields: schema_input
                .input_fields
                .as_ref()
                .expect("fields on input object")
                .iter()
                .filter_map(Option::as_ref)
                .map(|f| {
                    let name = f
                        .input_value
                        .name
                        .as_ref()
                        .expect("unnamed input object field")
                        .as_str();
                    let field = GqlObjectField {
                        description: None,
                        name: &name,
                        type_: f
                            .input_value
                            .type_
                            .as_ref()
                            .map(|s| s.into())
                            .expect("type on input object field"),
                        deprecation: DeprecationStatus::Current,
                    };
                    (name, field)
                })
                .collect(),
            is_required: false.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::*;
    use crate::field_type::FieldType;

    #[test]
    fn gql_input_to_rust() {
        let cat = GqlInput {
            description: None,
            name: "Cat",
            fields: vec![
                (
                    "pawsCount",
                    GqlObjectField {
                        description: None,
                        name: "pawsCount",
                        type_: FieldType::Named(float_type()),
                        deprecation: DeprecationStatus::Current,
                    },
                ),
                (
                    "offsprings",
                    GqlObjectField {
                        description: None,
                        name: "offsprings",
                        type_: FieldType::Vector(Box::new(FieldType::Named("Cat"))),
                        deprecation: DeprecationStatus::Current,
                    },
                ),
                (
                    "requirements",
                    GqlObjectField {
                        description: None,
                        name: "requirements",
                        type_: FieldType::Optional(Box::new(FieldType::Named("CatRequirements"))),
                        deprecation: DeprecationStatus::Current,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            is_required: false.into(),
        };

        let expected =  "# [ derive ( Clone , Serialize ) ] pub struct Cat { pub offsprings : Vec < Cat > , # [ serde ( rename = \"pawsCount\" ) ] pub paws_count : Float , # [ serde ( skip_serializing_if = \"Option::is_none\" ) ] pub requirements : Option < CatRequirements > , } impl Cat { pub fn new ( offsprings : Vec < Cat > , paws_count : Float ) -> Self { Self { offsprings : offsprings , paws_count : paws_count , requirements : None , } } }";
        let mut schema = crate::schema::Schema::new();
        schema.inputs.insert(cat.name, cat);
        let mut context = QueryContext::new_empty(&schema);
        context.ingest_input_derives("Clone").unwrap();

        assert_eq!(
            format!(
                "{}",
                context.schema.inputs["Cat"].to_rust(&context).unwrap()
            ),
            expected
        );
    }
}
