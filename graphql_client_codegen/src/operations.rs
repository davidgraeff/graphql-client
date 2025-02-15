use crate::constants::*;
use crate::query::QueryContext;
use crate::selection::Selection;
use crate::variables::Variable;
use graphql_parser::query::OperationDefinition;
use heck::SnakeCase;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;

#[derive(Debug, Clone)]
pub enum OperationType {
    Query,
    Mutation,
    Subscription,
}

#[derive(Debug, Clone)]
pub struct Operation<'query> {
    pub name: String,
    pub operation_type: OperationType,
    pub variables: Vec<Variable<'query>>,
    pub selection: Selection<'query>,
}

impl<'query> Operation<'query> {
    pub(crate) fn root_name<'schema>(
        &self,
        schema: &'schema crate::schema::Schema<'_>,
    ) -> &'schema str {
        match self.operation_type {
            OperationType::Query => schema.query_type.unwrap_or("Query"),
            OperationType::Mutation => schema.mutation_type.unwrap_or("Mutation"),
            OperationType::Subscription => schema.subscription_type.unwrap_or("Subscription"),
        }
    }

    pub(crate) fn is_subscription(&self) -> bool {
        match self.operation_type {
            OperationType::Subscription => true,
            _ => false,
        }
    }

    /// Generate the Variables structs fields. Used by expand_variables.
    pub(crate) fn variable_fields(&self, context: &QueryContext<'_, '_>) -> Vec<TokenStream> {
        self.variables.iter().map(|variable| {
            let ty = variable.ty.to_rust(context, "");
            let rust_safe_field_name =
                crate::shared::keyword_replace(&variable.name.to_snake_case());
            let mut rename =
                crate::shared::field_rename_annotation(&variable.name, &rust_safe_field_name);
            let name = Ident::new(&rust_safe_field_name, Span::call_site());

            if let crate::field_type::FieldType::Optional(_) = &variable.ty {
                rename = quote!(
                    #[serde(skip_serializing_if = "Option::is_none")]
                    #rename
                )
            }

            quote!(#rename pub #name: #ty)
        }).collect()
    }

    /// mark types of variables of this operation as required
    pub(crate) fn compute_variable_requirements(&self, context: &QueryContext<'_, '_>) {
        for variable in &self.variables {
            context.schema.require(&variable.ty.inner_name_str());
        }
    }

    /// Generate the Variables struct and all the necessary supporting code.
    pub(crate) fn expand_variables(&self, context: &QueryContext<'_, '_>) -> (TokenStream, Vec<TokenStream>, Vec<TokenStream>) {
        let variables = &self.variables;

        let variables_derives = context.variables_derives();

        if variables.is_empty() {
            return (variables_derives, vec![], vec![]);
        }

        let fields = self.variable_fields(context);

        let default_constructors = variables
            .iter()
            .map(|variable| variable.generate_default_value_constructor(context)).collect();

        (variables_derives, fields, default_constructors)
    }
}

impl<'query> ::std::convert::From<&'query OperationDefinition> for Operation<'query> {
    fn from(definition: &'query OperationDefinition) -> Operation<'query> {
        match *definition {
            OperationDefinition::Query(ref q) => Operation {
                name: q.name.clone().expect("unnamed operation"),
                operation_type: OperationType::Query,
                variables: q.variable_definitions.iter().map(|v| v.into()).collect(),
                selection: (&q.selection_set).into(),
            },
            OperationDefinition::Mutation(ref m) => Operation {
                name: m.name.clone().expect("unnamed operation"),
                operation_type: OperationType::Mutation,
                variables: m.variable_definitions.iter().map(|v| v.into()).collect(),
                selection: (&m.selection_set).into(),
            },
            OperationDefinition::Subscription(ref s) => Operation {
                name: s.name.clone().expect("unnamed operation"),
                operation_type: OperationType::Subscription,
                variables: s.variable_definitions.iter().map(|v| v.into()).collect(),
                selection: (&s.selection_set).into(),
            },
            OperationDefinition::SelectionSet(_) => panic!(SELECTION_SET_AT_ROOT),
        }
    }
}
