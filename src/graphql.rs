use crate::sql_types::*;
use cached::proc_macro::cached;
use cached::SizedCache;
use itertools::Itertools;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::ops::Deref;
use std::sync::Arc;

fn is_valid_graphql_name(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
        && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn to_base_type_name(name: &str, name_override: &Option<String>, inflect_names: bool) -> String {
    if let Some(name) = name_override {
        return name.to_string();
    };

    match inflect_names {
        false => name.to_string(),
        true => {
            let mut padded = "+".to_string();
            padded.push_str(name);

            // account_BY_email => Account_By_Email
            let casing: String = padded
                .chars()
                .zip(name.chars())
                .map(|(prev, cur)| match prev.is_alphanumeric() {
                    true => cur.to_string(),
                    false => cur.to_uppercase().to_string(),
                })
                .collect();

            str::replace(&casing, "_", "")
        }
    }
}

fn lowercase_first_letter(token: &str) -> String {
    let first_char = token.chars().next();
    match first_char {
        Some(c) => format!("{}{}", c.to_lowercase(), &token[c.len_utf8()..]),
        None => token.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_lowercase_first_letter() {
        fn run_test(token: &str, expected: &str) {
            let actual = super::lowercase_first_letter(token);
            assert_eq!(expected, actual);
        }

        run_test("Hello", "hello");
        run_test("用户", "用户");
    }
}

impl __Schema {
    fn inflect_names(&self, schema_oid: u32) -> bool {
        let schema = self.context.schemas.get(&schema_oid);
        schema.map(|s| s.directives.inflect_names).unwrap_or(false)
    }

    fn graphql_column_field_name(&self, column: &Column) -> String {
        if let Some(override_name) = &column.directives.name {
            return override_name.clone();
        }

        let inflect_names: bool = self.inflect_names(column.schema_oid);

        let base_type_name =
            to_base_type_name(&column.name, &column.directives.name, inflect_names);

        match inflect_names {
            // Lowercase first letter
            // AccountByEmail => accountByEmail
            true => lowercase_first_letter(&base_type_name),
            false => base_type_name,
        }
    }

    fn graphql_function_field_name(&self, function: &Function) -> String {
        if let Some(override_name) = &function.directives.name {
            return override_name.clone();
        }

        // remove underscore prefix from function name before inflecting
        let trimmed_function_name = &function.name.strip_prefix('_').unwrap_or(&function.name);

        let base_type_name = to_base_type_name(
            trimmed_function_name,
            &function.directives.name,
            self.inflect_names(function.schema_oid),
        );
        lowercase_first_letter(&base_type_name)
    }

    fn graphql_function_arg_name(&self, function: &Function, arg_name: &str) -> String {
        let base_type_name =
            to_base_type_name(arg_name, &None, self.inflect_names(function.schema_oid));
        lowercase_first_letter(&base_type_name)
    }

    fn graphql_enum_base_type_name(&self, enum_: &Enum, inflect_names: bool) -> String {
        to_base_type_name(&enum_.name, &enum_.directives.name, inflect_names)
    }

    fn graphql_table_base_type_name(&self, table: &Table) -> String {
        to_base_type_name(
            &table.name,
            &table.directives.name,
            self.inflect_names(table.schema_oid),
        )
    }

    fn graphql_foreign_key_field_name(&self, fkey: &ForeignKey, reverse_reference: bool) -> String {
        let mut table_ref: &ForeignKeyTableInfo = &fkey.referenced_table_meta;
        let mut name_override: &Option<String> = &fkey.directives.foreign_name;
        let mut is_unique: bool = true;
        let mut column_names: &Vec<String> = &fkey.local_table_meta.column_names;

        if reverse_reference {
            table_ref = &fkey.local_table_meta;
            name_override = &fkey.directives.local_name;
            is_unique = self.context.fkey_is_locally_unique(fkey);
            column_names = &fkey.referenced_table_meta.column_names;
        }

        let table: &Arc<Table> = self
            .context
            .get_table_by_oid(table_ref.oid)
            .expect("failed to get table by oid");

        let is_inflection_on = self.inflect_names(table.schema_oid);

        // If name is overridden, return immediately
        if let Some(name) = name_override {
            return name.to_string();
        }
        // "AccountHolder"
        let base_type_name =
            to_base_type_name(&table.name, &table.directives.name, is_inflection_on);

        // "accountHolder"
        let base_type_as_field_name = lowercase_first_letter(&base_type_name);

        let inflect_names: bool = self.inflect_names(table.schema_oid);

        let singular_name = match &column_names[..] {
            [column_name] => match is_inflection_on {
                true => match column_name.strip_suffix("_id") {
                    Some(column_name_stripped) => {
                        let base = to_base_type_name(column_name_stripped, &None, inflect_names);
                        lowercase_first_letter(&base)
                    }
                    None => base_type_as_field_name.clone(),
                },
                false => match column_name.strip_suffix("Id") {
                    Some(column_name_stripped) => {
                        let base = to_base_type_name(column_name_stripped, &None, inflect_names);
                        lowercase_first_letter(&base)
                    }
                    None => base_type_as_field_name.clone(),
                },
            },
            _ => base_type_as_field_name.clone(),
        };

        match is_unique {
            true => singular_name,
            false => format!("{base_type_as_field_name}Collection"),
        }
    }

    fn graphql_table_select_types_are_valid(&self, table: &Table) -> bool {
        let check1 = is_valid_graphql_name(&self.graphql_table_base_type_name(table));
        let check2 = table.primary_key().is_some();
        // Any column is selectable
        let check3 = table.is_any_column_selectable();

        check1 && check2 && check3
    }

    fn graphql_table_insert_types_are_valid(&self, table: &Table) -> bool {
        let check1 = self.graphql_table_select_types_are_valid(table);
        let check2 = table.is_any_column_insertable();
        check1 && check2
    }

    fn graphql_table_update_types_are_valid(&self, table: &Table) -> bool {
        let check1 = self.graphql_table_select_types_are_valid(table);
        let check2 = table.is_any_column_updatable();
        check1 && check2
    }

    fn graphql_table_delete_types_are_valid(&self, table: &Table) -> bool {
        let check1 = self.graphql_table_select_types_are_valid(table);
        let check2 = table.permissions.is_deletable;
        check1 && check2
    }
}

pub trait ___Type {
    // kind: __TypeKind!
    fn kind(&self) -> __TypeKind;

    // name: String
    fn name(&self) -> Option<String> {
        None
    }

    // description: String
    fn description(&self) -> Option<String> {
        None
    }

    // # OBJECT and INTERFACE only
    // fields(includeDeprecated: Boolean = false): [__Field!]
    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    // # OBJECT only
    // interfaces: [__Type!]
    fn interfaces(&self) -> Option<Vec<__Type>> {
        None
    }

    // # INTERFACE and UNION only
    // possibleTypes: [__Type!]
    fn possible_types(&self) -> Option<Vec<__Type>> {
        None
    }

    // # ENUM only
    // enumValues(includeDeprecated: Boolean = false): [__EnumValue!]
    fn enum_values(&self, _include_deprecated: bool) -> Option<Vec<__EnumValue>> {
        Some(vec![])
    }

    // # INPUT_OBJECT only
    // inputFields: [__InputValue!]
    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        None
    }

    // # NON_NULL and LIST only
    // ofType: __Type
    fn of_type(&self) -> Option<__Type> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct __Directive {
    pub name: String,
    pub description: Option<String>,
    pub locations: Vec<__DirectiveLocation>,
    pub args: Vec<__InputValue>,
    pub is_repeatable: bool,
}

impl __Directive {
    pub const TYPE: &'static str = "__Directive";

    // name: String!
    pub fn name(&self) -> &str {
        &self.name
    }

    // description: String
    pub fn description(&self) -> Option<&String> {
        self.description.as_ref()
    }

    // locations: [__DirectiveLocation!]!
    pub fn locations(&self) -> &[__DirectiveLocation] {
        &self.locations
    }

    // args: [__InputValue!]!
    pub fn args(&self) -> &[__InputValue] {
        &self.args
    }

    // isRepeatable: Boolean!
    pub fn is_repeatable(&self) -> bool {
        self.is_repeatable
    }
}

#[derive(Serialize, Clone, Debug)]
#[allow(dead_code)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum __DirectiveLocation {
    Query,
    Mutation,
    Subscription,
    Field,
    FragmentDefinition,
    FragmentSpread,
    InlineFragment,
    VariableDefinition,
    Schema,
    Scalar,
    Object,
    FieldDefinition,
    ArgumentDefinition,
    Interface,
    Union,
    Enum,
    EnumValue,
    InputObject,
    InputFieldDefinition,
}

pub trait ___Field {
    // name: String!
    fn name(&self) -> String;

    // description: String
    fn description(&self) -> Option<String>;

    // args: [__InputValue!]!
    fn args(&self) -> Vec<__InputValue>;

    // type: __Type!
    /// The literal introspection type, including type modifiers
    fn type_(&self) -> __Type;

    // isDeprecated: Boolean!
    fn is_deprecated(&self) -> bool {
        self.deprecation_reason().is_none()
    }

    // deprecationReason: String
    fn deprecation_reason(&self) -> Option<String> {
        None
    }
}

#[derive(Clone, Debug)]
pub enum NodeSQLType {
    Column(Arc<Column>),
    NodeId(Vec<Arc<Column>>),
    Function(Arc<Function>),
}

#[derive(Clone, Debug)]
pub struct __Field {
    pub name_: String,
    pub description: Option<String>,
    pub type_: __Type,
    pub args: Vec<__InputValue>,
    pub deprecation_reason: Option<String>,

    // Only set for Node types
    pub sql_type: Option<NodeSQLType>,
}

impl __Field {
    pub fn get_arg(&self, name: &str) -> Option<__InputValue> {
        for arg in &self.args {
            if arg.name().as_str() == name {
                return Some(arg.clone());
            }
        }
        None
    }
}

impl ___Field for __Field {
    // name: String!
    fn name(&self) -> String {
        self.name_.clone()
    }

    // description: String
    fn description(&self) -> Option<String> {
        self.description.clone()
    }

    // args: [__InputValue!]!
    fn args(&self) -> Vec<__InputValue> {
        self.args.clone()
    }

    // type: __Type!
    /// The literal introspection type, including type modifiers
    fn type_(&self) -> __Type {
        self.type_.clone()
    }

    // isDeprecated: Boolean!
    fn is_deprecated(&self) -> bool {
        self.deprecation_reason().is_some()
    }

    // deprecationReason: String
    fn deprecation_reason(&self) -> Option<String> {
        self.deprecation_reason.clone()
    }
}

#[derive(Clone, Debug)]
pub struct __InputValue {
    pub name_: String,
    pub type_: __Type,
    pub description: Option<String>,
    pub default_value: Option<String>,
    pub sql_type: Option<NodeSQLType>,
}

impl __InputValue {
    // name: String!
    pub fn name(&self) -> String {
        self.name_.clone()
    }

    // description: String
    pub fn description(&self) -> Option<String> {
        self.description.clone()
    }

    // type: __Type!
    pub fn type_(&self) -> __Type {
        self.type_.clone()
    }

    // defaultValue: String
    pub fn default_value(&self) -> Option<String> {
        self.default_value.clone()
    }

    // isDeprecated: Boolean!
    pub fn is_deprecated(&self) -> bool {
        self.deprecation_reason().is_some()
    }

    // deprecationReason: String
    pub fn deprecation_reason(&self) -> Option<String> {
        None
    }
}

#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum __TypeKind {
    SCALAR,
    OBJECT,
    INTERFACE,
    UNION,
    ENUM,
    INPUT_OBJECT,
    LIST,
    NON_NULL,
}

#[derive(Clone, Debug)]
pub struct __EnumValue {
    name: String,
    description: Option<String>,
    deprecation_reason: Option<String>,
}
impl __EnumValue {
    // name: String!
    pub fn name(&self) -> String {
        self.name.clone()
    }

    // description: String
    pub fn description(&self) -> Option<String> {
        self.description.clone()
    }

    // isDeprecated: Boolean!
    pub fn is_deprecated(&self) -> bool {
        self.deprecation_reason.is_some()
    }

    // deprecationReason: String
    pub fn deprecation_reason(&self) -> Option<String> {
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum __Type {
    Scalar(Scalar),
    //Composite(Composite),
    // Query
    Query(QueryType),
    Connection(ConnectionType),
    Edge(EdgeType),
    Node(NodeType),
    Enum(EnumType),
    NodeInterface(NodeInterfaceType),
    // Mutation
    Mutation(MutationType),
    InsertInput(InsertInputType),
    InsertResponse(InsertResponseType),
    UpdateInput(UpdateInputType),
    UpdateResponse(UpdateResponseType),
    DeleteResponse(DeleteResponseType),
    FuncCallResponse(FuncCallResponseType),
    OrderBy(OrderByType),
    OrderByEntity(OrderByEntityType),
    FilterType(FilterTypeType),
    FilterEntity(FilterEntityType),

    // Constant
    PageInfo(PageInfoType),
    // Introspection
    #[allow(clippy::enum_variant_names)]
    __TypeKind(__TypeKindType),
    __Schema(__SchemaType),
    #[allow(clippy::enum_variant_names)]
    __Type(__TypeType),
    __Field(__FieldType),
    __InputValue(__InputValueType),
    __EnumValue(__EnumValueType),
    __DirectiveLocation(__DirectiveLocationType),
    __Directive(__DirectiveType),
    // Modifiers
    List(ListType),
    NonNull(NonNullType),
    Aggregate(AggregateType),
    AggregateNumeric(AggregateNumericType),
}

#[cached(
    type = "SizedCache<u64, HashMap<String, __Field>>",
    create = "{ SizedCache::with_size(1000) }",
    convert = r#"{ calculate_hash(type_) }"#
)]
pub fn field_map(type_: &__Type) -> HashMap<String, __Field> {
    let mut hmap = HashMap::new();
    let fields = type_.fields(true).unwrap_or_default();
    for field in fields {
        hmap.insert(field.name(), field);
    }
    hmap.insert(
        "__typename".to_string(),
        __Field {
            name_: "__typename".to_string(),
            description: None,
            type_: __Type::Scalar(Scalar::String(None)),
            args: vec![],
            deprecation_reason: None,
            sql_type: None,
        },
    );
    hmap
}

#[cached(
    type = "SizedCache<u64, HashMap<String, __InputValue>>",
    create = "{ SizedCache::with_size(1000) }",
    convert = r#"{ calculate_hash(type_) }"#
)]
pub fn input_field_map(type_: &__Type) -> HashMap<String, __InputValue> {
    let mut hmap = HashMap::new();
    let fields = type_.input_fields().unwrap_or_default();
    for field in fields {
        hmap.insert(field.name(), field);
    }
    hmap
}

impl ___Type for __Type {
    // kind: __TypeKind!
    fn kind(&self) -> __TypeKind {
        match self {
            Self::Scalar(x) => x.kind(),
            Self::Enum(x) => x.kind(),
            Self::Query(x) => x.kind(),
            Self::Mutation(x) => x.kind(),
            Self::Connection(x) => x.kind(),
            Self::Edge(x) => x.kind(),
            Self::Node(x) => x.kind(),
            Self::NodeInterface(x) => x.kind(),
            Self::InsertInput(x) => x.kind(),
            Self::InsertResponse(x) => x.kind(),
            Self::UpdateInput(x) => x.kind(),
            Self::UpdateResponse(x) => x.kind(),
            Self::DeleteResponse(x) => x.kind(),
            Self::FuncCallResponse(x) => x.kind(),
            Self::FilterType(x) => x.kind(),
            Self::FilterEntity(x) => x.kind(),
            Self::OrderBy(x) => x.kind(),
            Self::OrderByEntity(x) => x.kind(),
            Self::PageInfo(x) => x.kind(),
            Self::__TypeKind(x) => x.kind(),
            Self::__Schema(x) => x.kind(),
            Self::__Type(x) => x.kind(),
            Self::__Field(x) => x.kind(),
            Self::__InputValue(x) => x.kind(),
            Self::__EnumValue(x) => x.kind(),
            Self::__DirectiveLocation(x) => x.kind(),
            Self::__Directive(x) => x.kind(),
            Self::List(x) => x.kind(),
            Self::NonNull(x) => x.kind(),
            Self::Aggregate(x) => x.kind(),
            Self::AggregateNumeric(x) => x.kind(),
        }
    }

    // name: String
    fn name(&self) -> Option<String> {
        match self {
            Self::Scalar(x) => x.name(),
            Self::Enum(x) => x.name(),
            Self::Query(x) => x.name(),
            Self::Mutation(x) => x.name(),
            Self::Connection(x) => x.name(),
            Self::Edge(x) => x.name(),
            Self::Node(x) => x.name(),
            Self::NodeInterface(x) => x.name(),
            Self::InsertInput(x) => x.name(),
            Self::InsertResponse(x) => x.name(),
            Self::UpdateInput(x) => x.name(),
            Self::UpdateResponse(x) => x.name(),
            Self::DeleteResponse(x) => x.name(),
            Self::FuncCallResponse(x) => x.name(),
            Self::FilterType(x) => x.name(),
            Self::FilterEntity(x) => x.name(),
            Self::OrderBy(x) => x.name(),
            Self::OrderByEntity(x) => x.name(),
            Self::PageInfo(x) => x.name(),
            Self::__TypeKind(x) => x.name(),
            Self::__Schema(x) => x.name(),
            Self::__Type(x) => x.name(),
            Self::__Field(x) => x.name(),
            Self::__InputValue(x) => x.name(),
            Self::__EnumValue(x) => x.name(),
            Self::__DirectiveLocation(x) => x.name(),
            Self::__Directive(x) => x.name(),
            Self::List(x) => x.name(),
            Self::NonNull(x) => x.name(),
            Self::Aggregate(x) => x.name(),
            Self::AggregateNumeric(x) => x.name(),
        }
    }

    // description: String
    fn description(&self) -> Option<String> {
        match self {
            Self::Scalar(x) => x.description(),
            Self::Enum(x) => x.description(),
            Self::Query(x) => x.description(),
            Self::Mutation(x) => x.description(),
            Self::Connection(x) => x.description(),
            Self::Edge(x) => x.description(),
            Self::Node(x) => x.description(),
            Self::NodeInterface(x) => x.description(),
            Self::InsertInput(x) => x.description(),
            Self::InsertResponse(x) => x.description(),
            Self::UpdateInput(x) => x.description(),
            Self::UpdateResponse(x) => x.description(),
            Self::DeleteResponse(x) => x.description(),
            Self::FuncCallResponse(x) => x.description(),
            Self::FilterType(x) => x.description(),
            Self::FilterEntity(x) => x.description(),
            Self::OrderBy(x) => x.description(),
            Self::OrderByEntity(x) => x.description(),
            Self::PageInfo(x) => x.description(),
            Self::__TypeKind(x) => x.description(),
            Self::__Schema(x) => x.description(),
            Self::__Type(x) => x.description(),
            Self::__Field(x) => x.description(),
            Self::__InputValue(x) => x.description(),
            Self::__EnumValue(x) => x.description(),
            Self::__DirectiveLocation(x) => x.description(),
            Self::__Directive(x) => x.description(),
            Self::List(x) => x.description(),
            Self::NonNull(x) => x.description(),
            Self::Aggregate(x) => x.description(),
            Self::AggregateNumeric(x) => x.description(),
        }
    }

    // # OBJECT and INTERFACE only
    // fields(includeDeprecated: Boolean = false): [__Field!]
    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        match self {
            Self::Scalar(x) => x.fields(_include_deprecated),
            Self::Enum(x) => x.fields(_include_deprecated),
            Self::Query(x) => x.fields(_include_deprecated),
            Self::Mutation(x) => x.fields(_include_deprecated),
            Self::Connection(x) => x.fields(_include_deprecated),
            Self::Edge(x) => x.fields(_include_deprecated),
            Self::Node(x) => x.fields(_include_deprecated),
            Self::NodeInterface(x) => x.fields(_include_deprecated),
            Self::InsertInput(x) => x.fields(_include_deprecated),
            Self::InsertResponse(x) => x.fields(_include_deprecated),
            Self::UpdateInput(x) => x.fields(_include_deprecated),
            Self::UpdateResponse(x) => x.fields(_include_deprecated),
            Self::DeleteResponse(x) => x.fields(_include_deprecated),
            Self::FuncCallResponse(x) => x.fields(_include_deprecated),
            Self::FilterType(x) => x.fields(_include_deprecated),
            Self::FilterEntity(x) => x.fields(_include_deprecated),
            Self::OrderBy(x) => x.fields(_include_deprecated),
            Self::OrderByEntity(x) => x.fields(_include_deprecated),
            Self::PageInfo(x) => x.fields(_include_deprecated),
            Self::__TypeKind(x) => x.fields(_include_deprecated),
            Self::__Schema(x) => x.fields(_include_deprecated),
            Self::__Type(x) => x.fields(_include_deprecated),
            Self::__Field(x) => x.fields(_include_deprecated),
            Self::__InputValue(x) => x.fields(_include_deprecated),
            Self::__EnumValue(x) => x.fields(_include_deprecated),
            Self::__DirectiveLocation(x) => x.fields(_include_deprecated),
            Self::__Directive(x) => x.fields(_include_deprecated),
            Self::List(x) => x.fields(_include_deprecated),
            Self::NonNull(x) => x.fields(_include_deprecated),
            Self::Aggregate(x) => x.fields(_include_deprecated),
            Self::AggregateNumeric(x) => x.fields(_include_deprecated),
        }
    }

    // # OBJECT only
    // interfaces: [__Type!]
    fn interfaces(&self) -> Option<Vec<__Type>> {
        match self {
            Self::Scalar(x) => x.interfaces(),
            Self::Enum(x) => x.interfaces(),
            Self::Query(x) => x.interfaces(),
            Self::Mutation(x) => x.interfaces(),
            Self::Connection(x) => x.interfaces(),
            Self::Edge(x) => x.interfaces(),
            Self::Node(x) => x.interfaces(),
            Self::NodeInterface(x) => x.interfaces(),
            Self::InsertInput(x) => x.interfaces(),
            Self::InsertResponse(x) => x.interfaces(),
            Self::UpdateInput(x) => x.interfaces(),
            Self::UpdateResponse(x) => x.interfaces(),
            Self::DeleteResponse(x) => x.interfaces(),
            Self::FuncCallResponse(x) => x.interfaces(),
            Self::FilterType(x) => x.interfaces(),
            Self::FilterEntity(x) => x.interfaces(),
            Self::OrderBy(x) => x.interfaces(),
            Self::OrderByEntity(x) => x.interfaces(),
            Self::PageInfo(x) => x.interfaces(),
            Self::__TypeKind(x) => x.interfaces(),
            Self::__Schema(x) => x.interfaces(),
            Self::__Type(x) => x.interfaces(),
            Self::__Field(x) => x.interfaces(),
            Self::__InputValue(x) => x.interfaces(),
            Self::__EnumValue(x) => x.interfaces(),
            Self::__DirectiveLocation(x) => x.interfaces(),
            Self::__Directive(x) => x.interfaces(),
            Self::List(x) => x.interfaces(),
            Self::NonNull(x) => x.interfaces(),
            Self::Aggregate(x) => x.interfaces(),
            Self::AggregateNumeric(x) => x.interfaces(),
        }
    }

    // # INTERFACE and UNION only
    // possibleTypes: [__Type!]
    fn possible_types(&self) -> Option<Vec<__Type>> {
        match self {
            Self::NodeInterface(x) => x.possible_types(),
            _ => None,
        }
    }

    // # ENUM only
    // enumValues(includeDeprecated: Boolean = false): [__EnumValue!]
    fn enum_values(&self, _include_deprecated: bool) -> Option<Vec<__EnumValue>> {
        match self {
            Self::Scalar(x) => x.enum_values(_include_deprecated),
            Self::Enum(x) => x.enum_values(_include_deprecated),
            Self::Query(x) => x.enum_values(_include_deprecated),
            Self::Mutation(x) => x.enum_values(_include_deprecated),
            Self::Connection(x) => x.enum_values(_include_deprecated),
            Self::Edge(x) => x.enum_values(_include_deprecated),
            Self::Node(x) => x.enum_values(_include_deprecated),
            Self::NodeInterface(x) => x.enum_values(_include_deprecated),
            Self::InsertInput(x) => x.enum_values(_include_deprecated),
            Self::InsertResponse(x) => x.enum_values(_include_deprecated),
            Self::UpdateInput(x) => x.enum_values(_include_deprecated),
            Self::UpdateResponse(x) => x.enum_values(_include_deprecated),
            Self::DeleteResponse(x) => x.enum_values(_include_deprecated),
            Self::FuncCallResponse(x) => x.enum_values(_include_deprecated),
            Self::FilterType(x) => x.enum_values(_include_deprecated),
            Self::FilterEntity(x) => x.enum_values(_include_deprecated),
            Self::OrderBy(x) => x.enum_values(_include_deprecated),
            Self::OrderByEntity(x) => x.enum_values(_include_deprecated),
            Self::PageInfo(x) => x.enum_values(_include_deprecated),
            Self::__TypeKind(x) => x.enum_values(_include_deprecated),
            Self::__Schema(x) => x.enum_values(_include_deprecated),
            Self::__Type(x) => x.enum_values(_include_deprecated),
            Self::__Field(x) => x.enum_values(_include_deprecated),
            Self::__InputValue(x) => x.enum_values(_include_deprecated),
            Self::__EnumValue(x) => x.enum_values(_include_deprecated),
            Self::__DirectiveLocation(x) => x.enum_values(_include_deprecated),
            Self::__Directive(x) => x.enum_values(_include_deprecated),
            Self::List(x) => x.enum_values(_include_deprecated),
            Self::NonNull(x) => x.enum_values(_include_deprecated),
            Self::Aggregate(x) => x.enum_values(_include_deprecated),
            Self::AggregateNumeric(x) => x.enum_values(_include_deprecated),
        }
    }

    // # INPUT_OBJECT only
    // inputFields: [__InputValue!]
    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        match self {
            Self::Scalar(x) => x.input_fields(),
            Self::Enum(x) => x.input_fields(),
            Self::Query(x) => x.input_fields(),
            Self::Mutation(x) => x.input_fields(),
            Self::Connection(x) => x.input_fields(),
            Self::Edge(x) => x.input_fields(),
            Self::Node(x) => x.input_fields(),
            Self::NodeInterface(x) => x.input_fields(),
            Self::InsertInput(x) => x.input_fields(),
            Self::InsertResponse(x) => x.input_fields(),
            Self::UpdateInput(x) => x.input_fields(),
            Self::UpdateResponse(x) => x.input_fields(),
            Self::DeleteResponse(x) => x.input_fields(),
            Self::FuncCallResponse(x) => x.input_fields(),
            Self::FilterType(x) => x.input_fields(),
            Self::FilterEntity(x) => x.input_fields(),
            Self::OrderBy(x) => x.input_fields(),
            Self::OrderByEntity(x) => x.input_fields(),
            Self::PageInfo(x) => x.input_fields(),
            Self::__TypeKind(x) => x.input_fields(),
            Self::__Schema(x) => x.input_fields(),
            Self::__Type(x) => x.input_fields(),
            Self::__Field(x) => x.input_fields(),
            Self::__InputValue(x) => x.input_fields(),
            Self::__EnumValue(x) => x.input_fields(),
            Self::__DirectiveLocation(x) => x.input_fields(),
            Self::__Directive(x) => x.input_fields(),
            Self::List(x) => x.input_fields(),
            Self::NonNull(x) => x.input_fields(),
            Self::Aggregate(x) => x.input_fields(),
            Self::AggregateNumeric(x) => x.input_fields(),
        }
    }

    // # NON_NULL and LIST only
    // ofType: __Type
    fn of_type(&self) -> Option<__Type> {
        match self {
            Self::List(x) => x.of_type(),
            Self::NonNull(x) => x.of_type(),
            _ => None,
        }
    }
}

impl __Type {
    /// Uwraps the List and NonNull modifiers to return a concrete __Type
    pub fn unmodified_type(&self) -> Self {
        match self {
            __Type::List(x) => x.type_.unmodified_type(),
            __Type::NonNull(x) => x.type_.unmodified_type(),
            _ => self.clone(),
        }
    }

    pub fn nullable_type(&self) -> Self {
        match self {
            __Type::NonNull(x) => (*x.type_).clone(),
            _ => self.clone(),
        }
    }

    pub fn return_type(&self) -> &Self {
        match self {
            __Type::FuncCallResponse(func_call_response_type) => {
                func_call_response_type.return_type.deref()
            }
            t => t,
        }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Scalar {
    ID,
    Int,
    Float,
    // The Option<u32> is an optional typmod for character length
    // to support e.g. char(2) and varchar(255) during input validation
    // It is not exposed in the GraphQL schema
    String(Option<i32>),
    Boolean,
    Date,
    Time,
    Datetime,
    BigInt,
    UUID,
    JSON,
    Cursor,
    BigFloat,
    // Unknown or unhandled types.
    // There is no guarentee how they will be serialized
    // and they can't be filtered or ordered
    Opaque,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __TypeKindType;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __SchemaType;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __TypeType;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __FieldType;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __InputValueType;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __EnumValueType;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __DirectiveLocationType;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __DirectiveType;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ListType {
    pub type_: Box<__Type>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct NonNullType {
    pub type_: Box<__Type>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SchemaType {
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct QueryType {
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MutationType {
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct InsertInputType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct UpdateInputType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct InsertResponseType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct UpdateResponseType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct DeleteResponseType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct FuncCallResponseType {
    pub function: Arc<Function>,
    pub schema: Arc<__Schema>,
    pub return_type: Box<__Type>,
}

impl FuncCallResponseType {
    pub fn inflected_to_sql_args(&self) -> HashMap<String, (String, String)> {
        let inflected_name_to_sql_name: HashMap<String, (String, String)> = self
            .function
            .args()
            .filter_map(|(_, arg_type_name, arg_name, _)| {
                arg_name.map(|arg_name| (arg_type_name, arg_name))
            })
            .map(|(arg_type_name, arg_name)| {
                (
                    self.schema
                        .graphql_function_arg_name(&self.function, arg_name),
                    (arg_type_name.to_string(), arg_name.to_string()),
                )
            })
            .collect();
        inflected_name_to_sql_name
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ForeignKeyReversible {
    pub fkey: Arc<ForeignKey>,
    pub reverse_reference: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ConnectionType {
    pub table: Arc<Table>,
    pub fkey: Option<ForeignKeyReversible>,

    pub schema: Arc<__Schema>,
}

impl ConnectionType {
    // default arguments for all connections
    fn get_connection_input_args(&self) -> Vec<__InputValue> {
        vec![
            __InputValue {
                name_: "first".to_string(),
                type_: __Type::Scalar(Scalar::Int),
                description: Some("Query the first `n` records in the collection".to_string()),
                default_value: None,
                sql_type: None,
            },
            __InputValue {
                name_: "last".to_string(),
                type_: __Type::Scalar(Scalar::Int),
                description: Some("Query the last `n` records in the collection".to_string()),
                default_value: None,
                sql_type: None,
            },
            __InputValue {
                name_: "before".to_string(),
                type_: __Type::Scalar(Scalar::Cursor),
                description: Some(
                    "Query values in the collection before the provided cursor".to_string(),
                ),
                default_value: None,
                sql_type: None,
            },
            __InputValue {
                name_: "after".to_string(),
                type_: __Type::Scalar(Scalar::Cursor),
                description: Some(
                    "Query values in the collection after the provided cursor".to_string(),
                ),
                default_value: None,
                sql_type: None,
            },
            __InputValue {
                name_: "offset".to_string(),
                type_: __Type::Scalar(Scalar::Int),
                description: Some(
                    "Skip n values from the after cursor. Alternative to cursor pagination. Backward pagination not supported.".to_string(),
                ),
                default_value: None,
                sql_type: None,
            },
            __InputValue {
                name_: "filter".to_string(),
                type_: __Type::FilterEntity(FilterEntityType {
                    table: Arc::clone(&self.table),
                    schema: self.schema.clone(),
                }),
                description: Some(
                    "Filters to apply to the results set when querying from the collection"
                        .to_string(),
                ),
                default_value: None,
                sql_type: None,
            },
            __InputValue {
                name_: "orderBy".to_string(),
                type_: __Type::List(ListType {
                    type_: Box::new(__Type::NonNull(NonNullType {
                        type_: Box::new(__Type::OrderByEntity(OrderByEntityType {
                            table: Arc::clone(&self.table),
                            schema: self.schema.clone(),
                        })),
                    })),
                }),
                description: Some("Sort order to apply to the collection".to_string()),
                default_value: None,
                sql_type: None,
            },
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum EnumSource {
    Enum(Arc<Enum>),
    FilterIs,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EnumType {
    pub enum_: EnumSource,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct OrderByType {}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct OrderByEntityType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum FilterableType {
    Scalar(Scalar),
    Enum(EnumType),
    List(ListType),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct FilterTypeType {
    pub entity: FilterableType,
    pub schema: Arc<__Schema>,
}

impl FilterTypeType {
    fn entity_name(&self) -> String {
        match &self.entity {
            FilterableType::Scalar(s) => s.name().expect("scalar name should exist"),
            FilterableType::Enum(e) => e.name().expect("enum type name should exist"),
            FilterableType::List(l) => format!(
                "{}List",
                l.of_type()
                    .expect("inner list type should exist")
                    .name()
                    .expect("inner list type name should exist")
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct FilterEntityType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EdgeType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeType {
    pub table: Arc<Table>,

    // If one is present, both should be present
    // could be improved
    pub fkey: Option<Arc<ForeignKey>>,
    pub reverse_reference: Option<bool>,

    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeInterfaceType {
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PageInfoType;

impl ___Type for QueryType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("Query".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("The root type for querying data".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        let mut f = Vec::new();
        let single_entrypoint = __Field {
            name_: "node".to_string(),
            type_: __Type::NodeInterface(NodeInterfaceType {
                schema: Arc::clone(&self.schema),
            }),
            args: vec![__InputValue {
                name_: "nodeId".to_string(),
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::ID)),
                }),
                description: Some("The record's `ID`".to_string()),
                default_value: None,
                sql_type: None,
            }],
            description: Some("Retrieve a record by its `ID`".to_string()),
            deprecation_reason: None,
            sql_type: None,
        };
        f.push(single_entrypoint);

        for table in self
            .schema
            .context
            .tables
            .values()
            .filter(|table| self.schema.graphql_table_select_types_are_valid(table))
        {
            {
                let table_base_type_name = &self.schema.graphql_table_base_type_name(table);

                let connection_type = ConnectionType {
                    table: Arc::clone(table),
                    fkey: None,
                    schema: Arc::clone(&self.schema),
                };

                let connection_args = connection_type.get_connection_input_args();

                let collection_entrypoint = __Field {
                    name_: format!("{}Collection", lowercase_first_letter(table_base_type_name)),
                    type_: __Type::Connection(connection_type),
                    args: connection_args,
                    description: Some(format!(
                        "A pagable collection of type `{}`",
                        table_base_type_name
                    )),
                    deprecation_reason: None,
                    sql_type: None,
                };

                f.push(collection_entrypoint);
            }
        }

        let existing_fields: HashSet<String> = f.iter().map(|f| f.name()).collect();

        let function_fields = function_fields(
            &self.schema,
            &[FunctionVolatility::Immutable, FunctionVolatility::Stable],
        );

        f.extend(
            function_fields
                .into_iter()
                .filter(|ff| !existing_fields.contains(&ff.name())),
        );

        // Default fields always preset
        f.extend(vec![
            __Field {
                name_: "__type".to_string(),
                type_: __Type::__Type(__TypeType),
                args: vec![__InputValue {
                    name_: "name".to_string(),
                    type_: __Type::Scalar(Scalar::String(None)),
                    description: None,
                    default_value: None,
                    sql_type: None,
                }],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                name_: "__schema".to_string(),
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::__Schema(__SchemaType)),
                }),
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
        ]);

        f.sort_by_key(|a| a.name());
        Some(f)
    }
}

fn function_fields(schema: &Arc<__Schema>, volatilities: &[FunctionVolatility]) -> Vec<__Field> {
    let sql_types = &schema.context.types;
    let function_name_to_count = Function::function_names_to_count(&schema.context.functions);
    schema
        .context
        .functions
        .iter()
        .filter(|func| func.is_supported(&schema.context, &function_name_to_count))
        .filter(|func| volatilities.contains(&func.volatility))
        .filter_map(|func| match sql_types.get(&func.type_oid) {
            None => None,
            Some(sql_type) => {
                if let Some(return_type) = sql_type.to_graphql_type(None, func.is_set_of, schema) {
                    let mut gql_args = function_args(schema, func);
                    if let __Type::Connection(connection_type) = &return_type {
                        let connection_args = connection_type.get_connection_input_args();
                        let connection_arg_names: HashSet<String> =
                            connection_args.iter().map(|arg| arg.name()).collect();
                        for arg in &gql_args {
                            if connection_arg_names.contains(&arg.name()) {
                                return None;
                            }
                        }
                        gql_args.extend(connection_args);
                    }

                    // If the return type is a table type, it must be selectable
                    if !match &return_type {
                        __Type::Node(table_type) => {
                            schema.graphql_table_select_types_are_valid(&table_type.table)
                        }
                        __Type::Connection(table_type) => {
                            schema.graphql_table_select_types_are_valid(&table_type.table)
                        }
                        _ => true,
                    } {
                        return None;
                    }

                    Some(__Field {
                        name_: schema.graphql_function_field_name(func),
                        type_: __Type::FuncCallResponse(FuncCallResponseType {
                            function: Arc::clone(func),
                            schema: Arc::clone(schema),
                            return_type: Box::new(return_type),
                        }),
                        args: gql_args,
                        description: func.directives.description.clone(),
                        deprecation_reason: None,
                        sql_type: Some(NodeSQLType::Function(Arc::clone(func))),
                    })
                } else {
                    None
                }
            }
        })
        .filter(|x| is_valid_graphql_name(&x.name_))
        .collect()
}

fn function_args(schema: &Arc<__Schema>, func: &Arc<Function>) -> Vec<__InputValue> {
    let sql_types = &schema.context.types;
    func.args()
        .filter(|(_, _, arg_name, _)| !arg_name.is_none())
        .filter_map(
            |(arg_type, _, arg_name, arg_default)| match sql_types.get(&arg_type) {
                Some(t) => {
                    if matches!(t.category, TypeCategory::Pseudo) {
                        None
                    } else {
                        Some((
                            t,
                            arg_name.expect("function arg name should exist"),
                            arg_default,
                        ))
                    }
                }
                None => None,
            },
        )
        .filter_map(|(arg_type, arg_name, arg_default)| {
            arg_type.to_graphql_type(None, false, schema).map(|t| {
                // wrap arg type in non-null if arg is not default
                let t = if arg_default.is_none() {
                    __Type::NonNull(NonNullType { type_: Box::new(t) })
                } else {
                    t
                };
                (t, arg_name, arg_default)
            })
        })
        .map(|(arg_type, arg_name, arg_default)| {
            let default_value = if let Some(default_value) = arg_default {
                match default_value {
                    DefaultValue::NonNull(value) => Some(value),
                    DefaultValue::Null => None,
                }
            } else {
                None
            };
            __InputValue {
                name_: schema.graphql_function_arg_name(func, arg_name),
                type_: arg_type,
                description: None,
                default_value,
                sql_type: None,
            }
        })
        .collect()
}

impl ___Type for MutationType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("Mutation".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("The root type for creating and mutating data".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        let mut f = Vec::new();

        // TODO, filter to types in type map in case any were filtered out
        for table in self.schema.context.tables.values() {
            let table_base_type_name = self.schema.graphql_table_base_type_name(table);

            if self.schema.graphql_table_insert_types_are_valid(table) {
                f.push(__Field {
                    name_: format!("insertInto{}Collection", table_base_type_name),
                    type_: __Type::InsertResponse(InsertResponseType {
                        table: Arc::clone(table),
                        schema: Arc::clone(&self.schema),
                    }),
                    args: vec![__InputValue {
                        name_: "objects".to_string(),
                        type_: __Type::NonNull(NonNullType {
                            type_: Box::new(__Type::List(ListType {
                                type_: Box::new(__Type::NonNull(NonNullType {
                                    type_: Box::new(__Type::InsertInput(InsertInputType {
                                        table: Arc::clone(table),
                                        schema: Arc::clone(&self.schema),
                                    })),
                                })),
                            })),
                        }),
                        description: None,
                        default_value: None,
                        sql_type: None,
                    }],
                    description: Some(format!(
                        "Adds one or more `{}` records to the collection",
                        table_base_type_name
                    )),
                    deprecation_reason: None,
                    sql_type: None,
                });
            }

            if self.schema.graphql_table_update_types_are_valid(table) {
                f.push(__Field {
                    name_: format!("update{}Collection", table_base_type_name),
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::UpdateResponse(UpdateResponseType {
                            table: Arc::clone(table),
                            schema: Arc::clone(&self.schema),
                        })),
                    }),
                    args: vec![
                        __InputValue {
                            name_: "set".to_string(),
                            type_: __Type::NonNull(NonNullType {
                                type_: Box::new(__Type::UpdateInput(UpdateInputType {
                                    table: Arc::clone(table),
                                    schema: Arc::clone(&self.schema),
                                })),
                            }),
                            description: Some("Fields that are set will be updated for all records matching the `filter`".to_string()),
                            default_value: None,
                            sql_type: None,
                        },
                        __InputValue {
                            name_: "filter".to_string(),
                            type_: __Type::FilterEntity(FilterEntityType {
                                table: Arc::clone(table),
                                schema: Arc::clone(&self.schema),
                            }),
                            description: Some("Restricts the mutation's impact to records matching the criteria".to_string()),
                            default_value: None,
                            sql_type: None,
                        },
                        __InputValue {
                            name_: "atMost".to_string(),
                            type_: __Type::NonNull(NonNullType {
                                type_: Box::new(__Type::Scalar(Scalar::Int)),
                            }),
                            description: Some("The maximum number of records in the collection permitted to be affected".to_string()),
                            default_value: Some("1".to_string()),
                            sql_type: None,
                        },
                    ],
                    description: Some(format!(
                        "Updates zero or more records in the `{}` collection",
                        table_base_type_name
                    )),
                    deprecation_reason: None,
                    sql_type: None,
                });
            }

            if self.schema.graphql_table_delete_types_are_valid(table) {
                f.push(__Field {
                    name_: format!("deleteFrom{}Collection", table_base_type_name),
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::DeleteResponse(DeleteResponseType {
                            table: Arc::clone(table),
                            schema: Arc::clone(&self.schema),
                        })),
                    }),
                    args: vec![
                        __InputValue {
                            name_: "filter".to_string(),
                            type_: __Type::FilterEntity(FilterEntityType {
                                table: Arc::clone(table),
                                schema: Arc::clone(&self.schema),
                            }),
                            description: Some(
                                "Restricts the mutation's impact to records matching the criteria"
                                    .to_string(),
                            ),
                            default_value: None,
                            sql_type: None,
                        },
                        __InputValue {
                            name_: "atMost".to_string(),
                            type_: __Type::NonNull(NonNullType {
                                type_: Box::new(__Type::Scalar(Scalar::Int)),
                            }),
                            description: Some("The maximum number of records in the collection permitted to be affected".to_string()),
                            default_value: Some("1".to_string()),
                            sql_type: None,
                        },
                    ],
                    description: Some(format!(
                        "Deletes zero or more records from the `{}` collection",
                        table_base_type_name
                    )),
                    deprecation_reason: None,
                    sql_type: None,
                })
            }
        }
        let existing_fields: HashSet<String> = f.iter().map(|f| f.name()).collect();

        let function_fields = function_fields(&self.schema, &[FunctionVolatility::Volatile]);

        f.extend(
            function_fields
                .into_iter()
                .filter(|ff| !existing_fields.contains(&ff.name())),
        );
        f.sort_by_key(|a| a.name());
        Some(f)
    }
}

impl ___Type for Scalar {
    fn kind(&self) -> __TypeKind {
        __TypeKind::SCALAR
    }

    fn name(&self) -> Option<String> {
        Some(
            match self {
                Self::ID => "ID",
                Self::Int => "Int",
                Self::Float => "Float",
                Self::String(_) => "String",
                Self::Boolean => "Boolean",
                Self::Datetime => "Datetime",
                Self::Date => "Date",
                Self::Time => "Time",
                Self::BigInt => "BigInt",
                Self::UUID => "UUID",
                Self::JSON => "JSON",
                Self::Cursor => "Cursor",
                Self::BigFloat => "BigFloat",
                Self::Opaque => "Opaque",
            }
            .to_string(),
        )
    }

    fn description(&self) -> Option<String> {
        Some(
            match self {
                Self::ID => "A globally unique identifier for a given record",
                Self::Int => "A scalar integer up to 32 bits",
                Self::Float => "A scalar floating point value up to 32 bits",
                Self::String(_) => "A string",
                Self::Boolean => "A value that is true or false",
                Self::BigInt => "An arbitrary size integer represented as a string",
                Self::Date => "A date without time information",
                Self::Time => "A time without date information",
                Self::Datetime => "A date and time",
                Self::UUID => "A universally unique identifier",
                Self::JSON => "A Javascript Object Notation value serialized as a string",
                Self::Cursor => {
                    "An opaque string using for tracking a position in results during pagination"
                }
                Self::BigFloat => "A high precision floating point value represented as a string",
                Self::Opaque => "Any type not handled by the type system",
            }
            .to_string(),
        )
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }
}

impl ___Type for EnumType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::ENUM
    }

    fn name(&self) -> Option<String> {
        match &self.enum_ {
            EnumSource::Enum(enum_) => {
                let inflect_names = self.schema.inflect_names(enum_.schema_oid);
                Some(
                    self.schema
                        .graphql_enum_base_type_name(enum_, inflect_names),
                )
            }
            EnumSource::FilterIs => Some("FilterIs".to_string()),
        }
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn enum_values(&self, _include_deprecated: bool) -> Option<Vec<__EnumValue>> {
        Some(match &self.enum_ {
            EnumSource::Enum(enum_) => enum_
                .values
                .iter()
                .map(|x| __EnumValue {
                    name: enum_
                        .directives
                        .mappings
                        .as_ref()
                        // Use mappings if available and mapped
                        .and_then(|mappings| mappings.get_by_left(&x.name).cloned())
                        .unwrap_or_else(|| x.name.clone()),
                    description: None,
                    deprecation_reason: None,
                })
                .collect(),
            EnumSource::FilterIs => {
                vec![
                    __EnumValue {
                        name: "NULL".to_string(),
                        description: None,
                        deprecation_reason: None,
                    },
                    __EnumValue {
                        name: "NOT_NULL".to_string(),
                        description: None,
                        deprecation_reason: None,
                    },
                ]
            }
        })
    }
}

impl ___Type for ConnectionType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}Connection",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        let table_base_type_name = &self.schema.graphql_table_base_type_name(&self.table);
        let edge_type = __Type::Edge(EdgeType {
            table: Arc::clone(&self.table),
            schema: self.schema.clone(),
        });

        let edge = __Field {
            name_: "edges".to_string(),
            type_: __Type::NonNull(NonNullType {
                type_: Box::new(__Type::List(ListType {
                    type_: Box::new(__Type::NonNull(NonNullType {
                        type_: Box::new(edge_type),
                    })),
                })),
            }),
            args: vec![],
            description: None,
            deprecation_reason: None,
            sql_type: None,
        };

        let page_info = __Field {
            name_: "pageInfo".to_string(),
            type_: __Type::NonNull(NonNullType {
                type_: Box::new(__Type::PageInfo(PageInfoType)),
            }),
            args: vec![],
            description: None,
            deprecation_reason: None,
            sql_type: None,
        };

        let mut fields = vec![edge, page_info];

        // Conditionally add totalCount based on the directive
        if let Some(total_count_directive) = self.table.directives.total_count.as_ref() {
            if total_count_directive.enabled {
                let total_count = __Field {
                    name_: "totalCount".to_string(),
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::Int)),
                    }),
                    args: vec![],
                    description: Some(
                        "The total number of records matching the `filter` criteria".to_string(),
                    ),
                    deprecation_reason: None,
                    sql_type: None,
                };
                fields.push(total_count);
            }
        }

        // Conditionally add aggregate based on the directive
        if let Some(aggregate_directive) = self.table.directives.aggregate.as_ref() {
            if aggregate_directive.enabled {
                let aggregate = __Field {
                    name_: "aggregate".to_string(),
                    type_: __Type::Aggregate(AggregateType {
                        table: Arc::clone(&self.table),
                        schema: self.schema.clone(),
                    }),
                    args: vec![],
                    description: Some(format!(
                        "Aggregate functions calculated on the collection of `{table_base_type_name}`"
                    )),
                    deprecation_reason: None,
                    sql_type: None,
                };
                fields.push(aggregate);
            }
        }

        Some(fields)
    }
}

impl ___Type for NodeInterfaceType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::INTERFACE
    }

    fn name(&self) -> Option<String> {
        Some("Node".to_string())
    }

    fn possible_types(&self) -> Option<Vec<__Type>> {
        let node_interface_name = self.name().expect("node interface type name should exist");

        let mut possible_types = vec![];

        // Use type_map(..) not types() because its cached
        for type_ in type_map(&self.schema)
            .into_values()
            .sorted_by(|a, b| a.name().cmp(&b.name()))
        {
            let type_interfaces: Vec<__Type> = type_.interfaces().unwrap_or(vec![]);
            let interface_names: Vec<String> = type_interfaces
                .iter()
                .map(|x| x.name().expect("type interface name should exist"))
                .collect();
            if interface_names.contains(&node_interface_name) {
                possible_types.push(type_)
            }
        }

        Some(possible_types)
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(vec![__Field {
            name_: "nodeId".to_string(),
            type_: __Type::NonNull(NonNullType {
                type_: Box::new(__Type::Scalar(Scalar::ID)),
            }),
            args: vec![],
            description: Some("Retrieves a record by `ID`".to_string()),
            deprecation_reason: None,
            sql_type: None,
        }])
    }
}

impl ___Type for EdgeType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}Edge",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(vec![
            __Field {
                name_: "cursor".to_string(),
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::String(None))),
                }),
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                name_: "node".to_string(),
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Node(NodeType {
                        table: Arc::clone(&self.table),
                        fkey: None,
                        reverse_reference: None,
                        schema: Arc::clone(&self.schema),
                    })),
                }),
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
        ])
    }
}

impl Type {
    fn to_graphql_type(
        &self,
        max_characters: Option<i32>,
        is_set_of: bool,
        schema: &Arc<__Schema>,
    ) -> Option<__Type> {
        if is_set_of && !(self.category == TypeCategory::Table) {
            // If a function returns a pseudotype with a single column
            // e.g. table( id int )
            // postgres records that in pg_catalog as returning a setof int
            // we don't support pseudo type returns, but this was sneaking through
            // because it looks like a concrete type
            return None;
        }

        match self.category {
            TypeCategory::Other => {
                Some(match self.oid {
                    20 => __Type::Scalar(Scalar::BigInt),       // bigint
                    16 => __Type::Scalar(Scalar::Boolean),      // boolean
                    1082 => __Type::Scalar(Scalar::Date),       // date
                    1184 => __Type::Scalar(Scalar::Datetime),   // timestamp with time zone
                    1114 => __Type::Scalar(Scalar::Datetime),   // timestamp without time zone
                    701 => __Type::Scalar(Scalar::Float),       // double precision
                    23 => __Type::Scalar(Scalar::Int),          // integer
                    21 => __Type::Scalar(Scalar::Int),          // smallint
                    700 => __Type::Scalar(Scalar::Float),       // real
                    3802 => __Type::Scalar(Scalar::JSON),       // jsonb
                    114 => __Type::Scalar(Scalar::JSON),        // json
                    1083 => __Type::Scalar(Scalar::Time),       // time without time zone
                    2950 => __Type::Scalar(Scalar::UUID),       // uuid
                    1700 => __Type::Scalar(Scalar::BigFloat),   // numeric
                    25 => __Type::Scalar(Scalar::String(None)), // text
                    // char, bpchar, varchar
                    18 | 1042 | 1043 => __Type::Scalar(Scalar::String(max_characters)),
                    _ => match self.name.as_str() {
                        // would be nice to do something better here like confirm the type came
                        // from an extension but until types from extensions become a bigger issue
                        // this avoids slowing down the sql context query
                        "citext" => __Type::Scalar(Scalar::String(None)),
                        _ => __Type::Scalar(Scalar::Opaque),
                    },
                })
            }
            TypeCategory::Array => match self.array_element_type_oid {
                Some(array_element_type_oid) => {
                    let sql_types = &schema.context.types;
                    let element_sql_type: Option<&Arc<Type>> =
                        sql_types.get(&array_element_type_oid);

                    let inner_graphql_type: __Type = match element_sql_type {
                        Some(sql_type) => match sql_type.permissions.is_usable {
                            true => match sql_type.to_graphql_type(None, false, schema) {
                                None => {
                                    return None;
                                }
                                Some(inner_type) => inner_type,
                            },
                            false => {
                                return None;
                            }
                        },
                        None => __Type::Scalar(Scalar::Opaque),
                    };
                    Some(__Type::List(ListType {
                        type_: Box::new(inner_graphql_type),
                    }))
                }
                // should not hpapen
                None => None,
            },
            TypeCategory::Enum => match schema.context.enums.get(&self.oid) {
                Some(enum_) => match schema.context.schemas.contains_key(&enum_.schema_oid) {
                    true => Some(__Type::Enum(EnumType {
                        enum_: EnumSource::Enum(Arc::clone(enum_)),
                        schema: schema.clone(),
                    })),
                    false => Some(__Type::Scalar(Scalar::Opaque)),
                },
                None => Some(__Type::Scalar(Scalar::Opaque)),
            },
            TypeCategory::Table => {
                match self.table_oid {
                    // Shouldn't happen
                    None => None,
                    Some(table_oid) => match schema.context.tables.get(&table_oid) {
                        // Can happen if search path doesn't include referenced table
                        None => None,
                        Some(table) => match is_set_of {
                            true => Some(__Type::Connection(ConnectionType {
                                table: Arc::clone(table),
                                fkey: None,
                                schema: Arc::clone(schema),
                            })),
                            false => Some(__Type::Node(NodeType {
                                table: Arc::clone(table),
                                fkey: None,
                                reverse_reference: None,
                                schema: Arc::clone(schema),
                            })),
                        },
                    },
                }
            }
            // Composites not yet supported
            TypeCategory::Composite => None,
            // Psudotypes like "record" are not supported
            TypeCategory::Pseudo => None,
        }
    }
}

pub fn sql_column_to_graphql_type(col: &Column, schema: &Arc<__Schema>) -> Option<__Type> {
    let sql_type = schema.context.types.get(&col.type_oid)?;
    let maybe_type_w_list_mod = sql_type.to_graphql_type(col.max_characters, false, schema);
    match maybe_type_w_list_mod {
        None => None,
        Some(type_with_list_mod) => match col.is_not_null {
            true => Some(__Type::NonNull(NonNullType {
                type_: Box::new(type_with_list_mod),
            })),
            _ => Some(type_with_list_mod),
        },
    }
}

impl NodeType {
    fn foreign_key_type(
        &self,
        fkey: &ForeignKey,
        type_: __Type,
        is_reverse_reference: bool,
    ) -> __Type {
        if fkey.local_table_meta.column_names.iter().any(|colname| {
            self.table
                .columns
                .iter()
                .any(|c| &c.name == colname && c.is_not_null)
                && !fkey.referenced_table_meta.is_rls_enabled
                && !is_reverse_reference
        }) {
            __Type::NonNull(NonNullType {
                type_: Box::new(type_),
            })
        } else {
            type_
        }
    }
}

impl ___Type for NodeType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(self.schema.graphql_table_base_type_name(&self.table))
    }

    fn description(&self) -> Option<String> {
        self.table.directives.description.clone()
    }

    fn interfaces(&self) -> Option<Vec<__Type>> {
        let mut interfaces = vec![];

        if self.table.primary_key().is_some() {
            interfaces.push(__Type::NodeInterface(NodeInterfaceType {
                schema: Arc::clone(&self.schema),
            }))
        }

        match interfaces.is_empty() {
            false => Some(interfaces),
            true => None,
        }
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        let column_fields = self
            .table
            .columns
            .iter()
            .filter(|x| x.permissions.is_selectable)
            .filter(|x| !self.schema.context.is_composite(x.type_oid))
            .filter_map(|col| {
                sql_column_to_graphql_type(col, &self.schema).map(|utype| __Field {
                    name_: self.schema.graphql_column_field_name(col),
                    type_: utype,
                    args: vec![],
                    description: col.directives.description.clone(),
                    deprecation_reason: None,
                    sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                })
            })
            .filter(|x| is_valid_graphql_name(&x.name_))
            .collect();

        // nodeId field
        let mut node_id_field: Vec<__Field> = vec![];

        if self.table.primary_key().is_some() {
            let node_id = __Field {
                name_: "nodeId".to_string(),
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::ID)),
                }),
                args: vec![],
                description: Some("Globally Unique Record Identifier".to_string()),
                deprecation_reason: None,
                sql_type: Some(NodeSQLType::NodeId(
                    self.table
                        .primary_key_columns()
                        .iter()
                        .map(|x| Arc::clone(x))
                        .collect::<Vec<Arc<Column>>>(),
                )),
            };
            node_id_field.push(node_id);
        };

        let sql_types = &self.schema.context.types;
        // Functions require selecting an entire row. the whole table must be selectable
        // for functions to work
        let mut function_fields: Vec<__Field> = vec![];
        if self.table.permissions.is_selectable {
            function_fields = self
                .table
                .functions
                .iter()
                .filter(|x| x.permissions.is_executable)
                .filter(|func| {
                    // TODO: remove in favor of making `to_sql_type` return an Option
                    // so we can optionally remove inappropriate types
                    match sql_types.get(&func.type_oid) {
                        None => true,
                        Some(sql_type) => {
                            // disallow pseudo types
                            !matches!(&sql_type.category, TypeCategory::Pseudo)
                        }
                    }
                })
                .filter_map(|func| match sql_types.get(&func.type_oid) {
                    None => None,
                    Some(sql_type) => {
                        if let Some(gql_ret_type) =
                            sql_type.to_graphql_type(None, func.is_set_of, &self.schema)
                        {
                            let gql_args = match &gql_ret_type {
                                __Type::Connection(connection_type) => {
                                    connection_type.get_connection_input_args()
                                }
                                _ => vec![],
                            };

                            Some(__Field {
                                name_: self.schema.graphql_function_field_name(func),
                                type_: gql_ret_type,
                                args: gql_args,
                                description: func.directives.description.clone(),
                                deprecation_reason: None,
                                sql_type: Some(NodeSQLType::Function(Arc::clone(func))),
                            })
                        } else {
                            None
                        }
                    }
                })
                .filter(|x| is_valid_graphql_name(&x.name_))
                .collect();
        }

        let mut relation_fields: Vec<__Field> = vec![];

        for fkey in self
            .schema
            .context
            .foreign_keys()
            .iter()
            .filter(|x| x.local_table_meta.oid == self.table.oid)
        {
            let reverse_reference = false;

            let foreign_table: Option<&Arc<Table>> = self
                .schema
                .context
                .get_table_by_oid(fkey.referenced_table_meta.oid);
            // this should never happen but if there is an unhandled edge case panic-ing here
            // would block
            if foreign_table.is_none() {
                continue;
            }
            let foreign_table = foreign_table.expect("foreign table should exist");
            if !self
                .schema
                .graphql_table_select_types_are_valid(foreign_table)
            {
                continue;
            }

            let type_ = self.foreign_key_type(
                fkey,
                __Type::Node(NodeType {
                    table: Arc::clone(foreign_table),
                    fkey: Some(Arc::clone(fkey)),
                    reverse_reference: Some(reverse_reference),
                    schema: Arc::clone(&self.schema),
                }),
                reverse_reference,
            );

            let relation_field = __Field {
                name_: self
                    .schema
                    .graphql_foreign_key_field_name(fkey, reverse_reference),
                type_,
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            };
            relation_fields.push(relation_field);
        }

        for fkey in self
            .schema
            .context
            .foreign_keys()
            .iter()
            // inbound references
            .filter(|x| x.referenced_table_meta.oid == self.table.oid)
        {
            let reverse_reference = true;
            let foreign_table: Option<&Arc<Table>> = self
                .schema
                .context
                .get_table_by_oid(fkey.local_table_meta.oid);
            // this should never happen but if there is an unhandled edge case panic-ing here
            // would block
            if foreign_table.is_none() {
                continue;
            }
            let foreign_table = foreign_table.expect("foreign table should exist");
            if !self
                .schema
                .graphql_table_select_types_are_valid(foreign_table)
            {
                continue;
            }

            let relation_field = match self.schema.context.fkey_is_locally_unique(fkey) {
                false => {
                    let connection_type = ConnectionType {
                        table: Arc::clone(foreign_table),
                        fkey: Some(ForeignKeyReversible {
                            fkey: Arc::clone(fkey),
                            reverse_reference,
                        }),
                        schema: Arc::clone(&self.schema),
                    };
                    let connection_args = connection_type.get_connection_input_args();

                    let type_ = self.foreign_key_type(
                        fkey,
                        __Type::Connection(connection_type),
                        reverse_reference,
                    );

                    __Field {
                        name_: self
                            .schema
                            .graphql_foreign_key_field_name(fkey, reverse_reference),
                        type_,
                        args: connection_args,
                        description: None,
                        deprecation_reason: None,
                        sql_type: None,
                    }
                }
                true => {
                    let type_ = self.foreign_key_type(
                        fkey,
                        __Type::Node(NodeType {
                            table: Arc::clone(foreign_table),
                            fkey: Some(Arc::clone(fkey)),
                            reverse_reference: Some(reverse_reference),
                            schema: Arc::clone(&self.schema),
                        }),
                        reverse_reference,
                    );

                    __Field {
                        name_: self
                            .schema
                            .graphql_foreign_key_field_name(fkey, reverse_reference),
                        type_,
                        args: vec![],
                        description: None,
                        deprecation_reason: None,
                        sql_type: None,
                    }
                }
            };
            relation_fields.push(relation_field);
        }

        Some(
            vec![
                node_id_field,
                column_fields,
                relation_fields,
                function_fields,
            ]
            .into_iter()
            .flatten()
            //.sorted_by(|a, b| a.name().cmp(&b.name()))
            .collect(),
        )
    }
}

impl ___Type for PageInfoType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("PageInfo".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(vec![
            __Field {
                name_: "endCursor".to_string(),
                type_: __Type::Scalar(Scalar::String(None)),
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                name_: "hasNextPage".to_string(),
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                }),
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                name_: "hasPreviousPage".to_string(),
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                }),
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                name_: "startCursor".to_string(),
                type_: __Type::Scalar(Scalar::String(None)),
                args: vec![],
                description: None,
                deprecation_reason: None,
                sql_type: None,
            },
        ])
    }
}

impl ___Type for __TypeKindType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::ENUM
    }

    fn name(&self) -> Option<String> {
        Some("__TypeKind".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("An enum describing what kind of type a given `__Type` is.".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn enum_values(&self, _include_deprecated: bool) -> Option<Vec<__EnumValue>> {
        Some(vec![
            __EnumValue {
                name: format!("{:?}", __TypeKind::SCALAR),
                description: None,
                deprecation_reason: None,
            },
            __EnumValue {
                name: format!("{:?}", __TypeKind::OBJECT),
                description: None,
                deprecation_reason: None,
            },
            __EnumValue {
                name: format!("{:?}", __TypeKind::INTERFACE),
                description: None,
                deprecation_reason: None,
            },
            __EnumValue {
                name: format!("{:?}", __TypeKind::UNION),
                description: None,
                deprecation_reason: None,
            },
            __EnumValue {
                name: format!("{:?}", __TypeKind::ENUM),
                description: None,
                deprecation_reason: None,
            },
            __EnumValue {
                name: format!("{:?}", __TypeKind::INPUT_OBJECT),
                description: None,
                deprecation_reason: None,
            },
            __EnumValue {
                name: format!("{:?}", __TypeKind::LIST),
                description: None,
                deprecation_reason: None,
            },
            __EnumValue {
                name: format!("{:?}", __TypeKind::NON_NULL),
                description: None,
                deprecation_reason: None,
            },
        ])
    }
}

impl ___Type for __DirectiveLocationType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::ENUM
    }

    fn name(&self) -> Option<String> {
        Some("__DirectiveLocation".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("A Directive can be adjacent to many parts of the GraphQL language, a __DirectiveLocation describes one such possible adjacencies.".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn enum_values(&self, _include_deprecated: bool) -> Option<Vec<__EnumValue>> {
        Some(vec![
            __EnumValue {
                name: "QUERY".to_string(),
                description: Some("Location adjacent to a query operation.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "MUTATION".to_string(),
                description: Some("Location adjacent to a mutation operation.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "SUBSCRIPTION".to_string(),
                description: Some("Location adjacent to a subscription operation.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "FIELD".to_string(),
                description: Some("Location adjacent to a field.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "FRAGMENT_DEFINITION".to_string(),
                description: Some("Location adjacent to a fragment definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "FRAGMENT_SPREAD".to_string(),
                description: Some("Location adjacent to a fragment spread.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "INLINE_FRAGMENT".to_string(),
                description: Some("Location adjacent to an inline fragment.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "VARIABLE_DEFINITION".to_string(),
                description: Some("Location adjacent to a variable definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "SCHEMA".to_string(),
                description: Some("Location adjacent to a schema definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "SCALAR".to_string(),
                description: Some("Location adjacent to a scalar definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "OBJECT".to_string(),
                description: Some("Location adjacent to an object type definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "FIELD_DEFINITION".to_string(),
                description: Some("Location adjacent to a field definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "ARGUMENT_DEFINITION".to_string(),
                description: Some("Location adjacent to an argument definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "INTERFACE".to_string(),
                description: Some("Location adjacent to an interface definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "UNION".to_string(),
                description: Some("Location adjacent to a union definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "ENUM".to_string(),
                description: Some("Location adjacent to an enum definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "ENUM_VALUE".to_string(),
                description: Some("Location adjacent to an enum value definition.".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "INPUT_OBJECT".to_string(),
                description: Some(
                    "Location adjacent to an input object type definition.".to_string(),
                ),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "INPUT_FIELD_DEFINITION".to_string(),
                description: Some(
                    "Location adjacent to an input object field definition.".to_string(),
                ),
                deprecation_reason: None,
            },
        ])
    }
}

impl ___Type for __SchemaType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("__Schema".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("A GraphQL Schema defines the capabilities of a GraphQL server. It exposes all available types and directives on the server, as well as the entry points for query, mutation, and subscription operations.".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(
            vec![
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::List(ListType {
                            type_: Box::new(__Type::NonNull(NonNullType {
                                type_: Box::new(__Type::__Type(__TypeType {})),
                            })),
                        })),
                    }),
                    name_: "types".to_string(),
                    args: vec![],
                    description: Some("A list of all types supported by this server.".to_string()),
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                            type_: Box::new(__Type::__Type(__TypeType {})),
                    }),
                    name_: "queryType".to_string(),
                    args: vec![],
                    description: Some("The type that query operations will be rooted at.".to_string()),
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::__Type(__TypeType {}),
                    name_: "mutationType".to_string(),
                    args: vec![],
                    description: Some("If this server supports mutation, the type that mutation operations will be rooted at.".to_string()),
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::__Type(__TypeType {}),

                    name_: "subscriptionType".to_string(),
                    args: vec![],
                    description: Some("If this server support subscription, the type that subscription operations will be rooted at.".to_string()),
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::List(ListType {
                            type_: Box::new(__Type::NonNull(NonNullType {
                                type_: Box::new(__Type::__Directive(__DirectiveType {})),
                            })),
                        })),
                    }),
                    name_: "directives".to_string(),
                    args: vec![__InputValue {
                        name_: "includeDeprecated".to_string(),
                        type_: __Type::Scalar(Scalar::Boolean),
                        description: None,
                        default_value: Some("false".to_string()),
                        sql_type: None,
                    }],
                    description: Some(
                        "A list of all directives supported by this server.".to_string(),
                    ),
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "description".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },

            ]
            .into_iter()
            .sorted_by(|a, b| a.name().cmp(&b.name()))
            .collect(),
        )
    }
}

impl ___Type for __InputValueType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("__InputValue".to_string())
    }

    fn description(&self) -> Option<String> {
        Some(
            "Arguments provided to Fields or Directives and the input fields of an InputObject are represented as Input Values which describe their type and optionally a default value."
                .to_string(),
        )
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(
            vec![
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::String(None))),
                    }),
                    name_: "name".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "description".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::__Type(__TypeType)),
                    }),
                    name_: "type".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "defaultValue".to_string(),
                    args: vec![],
                    description: Some("A GraphQL-formatted string representing the default value for this input value.".to_string()),
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                    }),
                    name_: "isDeprecated".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "deprecationReason".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
            ]
            .into_iter()
            .sorted_by(|a, b| a.name().cmp(&b.name()))
            .collect(),
        )
    }
}

impl ___Type for __TypeType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("__Type".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("The fundamental unit of any GraphQL Schema is the type. There are many kinds of types in GraphQL as represented by the `__TypeKind` enum.\\n\\nDepending on the kind of a type, certain fields describe information about that type. Scalar types provide no information beyond a name, description and optional `specifiedByURL`, while Enum types provide their values. Object and Interface types provide the fields they describe. Abstract types, Union and Interface, provide the Object types possible at runtime. List and NonNull types compose other types ".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(
            vec![
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "name".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "description".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::__TypeKind(__TypeKindType)),
                    }),
                    name_: "kind".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::__InputValue(__InputValueType)),
                        })),
                    }),
                    name_: "inputFields".to_string(),
                    args: vec![__InputValue {
                        name_: "includeDeprecated".to_string(),
                        type_: __Type::Scalar(Scalar::Boolean),
                        description: None,
                        default_value: Some("false".to_string()),
                        sql_type: None,
                    }],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::__Type(__TypeType)),
                        })),
                    }),
                    name_: "interfaces".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::__Type(__TypeType)),
                        })),
                    }),
                    name_: "possibleTypes".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                // Declared as nullable list in introspection but GraphiQL query fails
                // if null value is returned
                __Field {
                    type_: __Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::__EnumValue(__EnumValueType {})),
                        })),
                    }),
                    name_: "enumValues".to_string(),
                    args: vec![__InputValue {
                        name_: "includeDeprecated".to_string(),
                        type_: __Type::Scalar(Scalar::Boolean),
                        description: None,
                        default_value: Some("false".to_string()),
                        sql_type: None,
                    }],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::__Field(__FieldType {})),
                        })),
                    }),
                    name_: "fields".to_string(),
                    args: vec![__InputValue {
                        name_: "includeDeprecated".to_string(),
                        type_: __Type::Scalar(Scalar::Boolean),
                        description: None,
                        default_value: Some("false".to_string()),
                        sql_type: None,
                    }],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::__Type(__TypeType),
                    name_: "ofType".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "specifiedByURL".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
            ]
            .into_iter()
            .sorted_by(|a, b| a.name().cmp(&b.name()))
            .collect(),
        )
    }
}

impl ___Type for __FieldType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("__Field".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("Object and Interface types are described by a list of Fields, each of which has a name, potentially a list of arguments, and a return type.".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(
            vec![
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::String(None))),
                    }),
                    name_: "name".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "description".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::List(ListType {
                            type_: Box::new(__Type::NonNull(NonNullType {
                                type_: Box::new(__Type::__InputValue(__InputValueType)),
                            })),
                        })),
                    }),
                    name_: "args".to_string(),
                    args: vec![__InputValue {
                        name_: "includeDeprecated".to_string(),
                        type_: __Type::Scalar(Scalar::Boolean),
                        description: None,
                        default_value: Some("false".to_string()),
                        sql_type: None,
                    }],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::__Type(__TypeType)),
                    }),
                    name_: "type".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                    }),
                    name_: "isDeprecated".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "deprecationReason".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
            ]
            .into_iter()
            .sorted_by(|a, b| a.name().cmp(&b.name()))
            .collect(),
        )
    }
}

impl ___Type for __EnumValueType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("__EnumValue".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("One possible value for a given Enum. Enum values are unique values, not a placeholder for a string or numeric value. However an Enum value is returned in a JSON response as a string.".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(
            vec![
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::String(None))),
                    }),
                    name_: "name".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "description".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                    }),
                    name_: "isDeprecated".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "deprecationReason".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
            ]
            .into_iter()
            .sorted_by(|a, b| a.name().cmp(&b.name()))
            .collect(),
        )
    }
}

impl ___Type for __DirectiveType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some("__Directive".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("A Directive provides a way to describe alternate runtime execution and type validation behavior in a GraphQL document.\\n\\nIn some cases, you need to provide options to alter GraphQL execution behavior in ways field arguments will not suffice, such as conditionally including or skipping a field. Directives provide this by describing additional information to the executor.".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(
            vec![
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::String(None))),
                    }),
                    name_: "name".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::Scalar(Scalar::String(None)),
                    name_: "description".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                    }),
                    name_: "isRepeatable".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::List(ListType {
                            type_: Box::new(__Type::NonNull(NonNullType {
                                type_: Box::new(__Type::__DirectiveLocation(
                                    __DirectiveLocationType,
                                )),
                            })),
                        })),
                    }),
                    name_: "locations".to_string(),
                    args: vec![],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
                __Field {
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::List(ListType {
                            type_: Box::new(__Type::NonNull(NonNullType {
                                type_: Box::new(__Type::__InputValue(__InputValueType)),
                            })),
                        })),
                    }),
                    name_: "args".to_string(),
                    args: vec![__InputValue {
                        name_: "includeDeprecated".to_string(),
                        type_: __Type::Scalar(Scalar::Boolean),
                        description: None,
                        default_value: Some("false".to_string()),
                        sql_type: None,
                    }],
                    description: None,
                    deprecation_reason: None,
                    sql_type: None,
                },
            ]
            .into_iter()
            .sorted_by(|a, b| a.name().cmp(&b.name()))
            .collect(),
        )
    }
}

impl ___Type for ListType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::LIST
    }

    fn name(&self) -> Option<String> {
        None
    }

    fn of_type(&self) -> Option<__Type> {
        Some((*(self.type_)).clone())
    }
}

impl ___Type for NonNullType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::NON_NULL
    }

    fn name(&self) -> Option<String> {
        None
    }

    fn of_type(&self) -> Option<__Type> {
        Some((*(self.type_)).clone())
    }
}

impl ___Type for InsertInputType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::INPUT_OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}InsertInput",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        Some(
            self.table
                .columns
                .iter()
                .filter(|x| x.permissions.is_insertable)
                .filter(|x| !x.is_generated)
                .filter(|x| !x.is_serial)
                .filter(|x| !self.schema.context.is_composite(x.type_oid))
                .filter_map(|col| {
                    sql_column_to_graphql_type(col, &self.schema).map(|utype| __InputValue {
                        name_: self.schema.graphql_column_field_name(col),
                        // If triggers are involved, we can't detect if a field is non-null. Default
                        // all fields to non-null and let postgres errors handle it.
                        type_: utype.nullable_type(),
                        description: None,
                        default_value: None,
                        sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                    })
                })
                .collect(),
        )
    }
}

impl ___Type for InsertResponseType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}InsertResponse",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(vec![
            __Field {
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::Int)),
                }),
                name_: "affectedCount".to_string(),
                args: vec![],
                description: Some("Count of the records impacted by the mutation".to_string()),
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::Node(NodeType {
                                table: Arc::clone(&self.table),
                                fkey: None,
                                reverse_reference: None,
                                schema: Arc::clone(&self.schema),
                            })),
                        })),
                    })),
                }),
                name_: "records".to_string(),
                args: vec![],
                description: Some("Array of records impacted by the mutation".to_string()),
                deprecation_reason: None,
                sql_type: None,
            },
        ])
    }
}

impl ___Type for UpdateInputType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::INPUT_OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}UpdateInput",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        Some(
            self.table
                .columns
                .iter()
                .filter(|x| x.permissions.is_updatable)
                .filter(|x| !x.is_generated)
                .filter(|x| !x.is_serial)
                .filter(|x| !self.schema.context.is_composite(x.type_oid))
                .filter_map(|col| {
                    sql_column_to_graphql_type(col, &self.schema).map(|utype| __InputValue {
                        name_: self.schema.graphql_column_field_name(col),
                        // TODO: handle possible array inputs
                        type_: utype.nullable_type(),
                        description: None,
                        default_value: None,
                        sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                    })
                })
                .collect(),
        )
    }
}

impl ___Type for UpdateResponseType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}UpdateResponse",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(vec![
            __Field {
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::Int)),
                }),
                name_: "affectedCount".to_string(),
                args: vec![],
                description: Some("Count of the records impacted by the mutation".to_string()),
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::Node(NodeType {
                                table: Arc::clone(&self.table),
                                fkey: None,
                                reverse_reference: None,
                                schema: Arc::clone(&self.schema),
                            })),
                        })),
                    })),
                }),
                name_: "records".to_string(),
                args: vec![],
                description: Some("Array of records impacted by the mutation".to_string()),
                deprecation_reason: None,
                sql_type: None,
            },
        ])
    }
}

impl ___Type for DeleteResponseType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}DeleteResponse",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        Some(vec![
            __Field {
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::Scalar(Scalar::Int)),
                }),
                name_: "affectedCount".to_string(),
                args: vec![],
                description: Some("Count of the records impacted by the mutation".to_string()),
                deprecation_reason: None,
                sql_type: None,
            },
            __Field {
                type_: __Type::NonNull(NonNullType {
                    type_: Box::new(__Type::List(ListType {
                        type_: Box::new(__Type::NonNull(NonNullType {
                            type_: Box::new(__Type::Node(NodeType {
                                table: Arc::clone(&self.table),
                                fkey: None,
                                reverse_reference: None,
                                schema: Arc::clone(&self.schema),
                            })),
                        })),
                    })),
                }),
                name_: "records".to_string(),
                args: vec![],
                description: Some("Array of records impacted by the mutation".to_string()),
                deprecation_reason: None,
                sql_type: None,
            },
        ])
    }
}

impl ___Type for FuncCallResponseType {
    fn kind(&self) -> __TypeKind {
        self.return_type.kind()
    }

    fn name(&self) -> Option<String> {
        self.return_type.name()
    }

    fn description(&self) -> Option<String> {
        self.return_type.description()
    }

    fn enum_values(&self, include_deprecated: bool) -> Option<Vec<__EnumValue>> {
        self.return_type.enum_values(include_deprecated)
    }

    fn fields(&self, include_deprecated: bool) -> Option<Vec<__Field>> {
        self.return_type.fields(include_deprecated)
    }

    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        self.return_type.input_fields()
    }

    fn interfaces(&self) -> Option<Vec<__Type>> {
        self.return_type.interfaces()
    }

    fn of_type(&self) -> Option<__Type> {
        self.return_type.of_type()
    }

    fn possible_types(&self) -> Option<Vec<__Type>> {
        self.return_type.possible_types()
    }
}

use std::str::FromStr;
use std::string::ToString;

#[derive(Clone, Copy, Debug)]
pub enum FilterOp {
    Equal,
    NotEqual,
    LessThan,
    LessThanEqualTo,
    GreaterThan,
    GreaterThanEqualTo,
    In,
    Is,
    StartsWith,
    Like,
    ILike,
    RegEx,
    IRegEx,
    Contains,
    ContainedBy,
    Overlap,
}

impl Display for FilterOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let res = match self {
            Self::Equal => "eq",
            Self::NotEqual => "neq",
            Self::LessThan => "lt",
            Self::LessThanEqualTo => "lte",
            Self::GreaterThan => "gt",
            Self::GreaterThanEqualTo => "gte",
            Self::In => "in",
            Self::Is => "is",
            Self::StartsWith => "startsWith",
            Self::Like => "like",
            Self::ILike => "ilike",
            Self::RegEx => "regex",
            Self::IRegEx => "iregex",
            Self::Contains => "contains",
            Self::ContainedBy => "containedBy",
            Self::Overlap => "overlaps",
        };
        write!(f, "{res}")
    }
}

impl FromStr for FilterOp {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "eq" => Ok(Self::Equal),
            "neq" => Ok(Self::NotEqual),
            "lt" => Ok(Self::LessThan),
            "lte" => Ok(Self::LessThanEqualTo),
            "gt" => Ok(Self::GreaterThan),
            "gte" => Ok(Self::GreaterThanEqualTo),
            "in" => Ok(Self::In),
            "is" => Ok(Self::Is),
            "startsWith" => Ok(Self::StartsWith),
            "like" => Ok(Self::Like),
            "ilike" => Ok(Self::ILike),
            "regex" => Ok(Self::RegEx),
            "iregex" => Ok(Self::IRegEx),
            "contains" => Ok(Self::Contains),
            "containedBy" => Ok(Self::ContainedBy),
            "overlaps" => Ok(Self::Overlap),
            _ => Err("Invalid filter operation".to_string()),
        }
    }
}

impl ___Type for FilterTypeType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::INPUT_OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!("{}Filter", self.entity_name()))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn description(&self) -> Option<String> {
        Some(format!(
            "Boolean expression comparing fields on type \"{}\"",
            self.entity_name()
        ))
    }

    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        let mut infields: Vec<__InputValue> = match &self.entity {
            FilterableType::Scalar(scalar) => {
                let supported_ops = match scalar {
                    // IDFilter only supports equality
                    Scalar::ID => vec![FilterOp::Equal],
                    // UUIDs are not ordered
                    Scalar::UUID => {
                        vec![
                            FilterOp::Equal,
                            FilterOp::NotEqual,
                            FilterOp::In,
                            FilterOp::Is,
                        ]
                    }
                    Scalar::Boolean => vec![FilterOp::Equal, FilterOp::Is],
                    Scalar::Int => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                    ],
                    Scalar::Float => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                    ],
                    Scalar::String(_) => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                        FilterOp::StartsWith,
                        FilterOp::Like,
                        FilterOp::ILike,
                        FilterOp::RegEx,
                        FilterOp::IRegEx,
                    ],
                    Scalar::BigInt => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                    ],
                    Scalar::Date => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                    ],
                    Scalar::Time => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                    ],
                    Scalar::Datetime => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                    ],
                    Scalar::BigFloat => vec![
                        FilterOp::Equal,
                        FilterOp::NotEqual,
                        FilterOp::LessThan,
                        FilterOp::LessThanEqualTo,
                        FilterOp::GreaterThan,
                        FilterOp::GreaterThanEqualTo,
                        FilterOp::In,
                        FilterOp::Is,
                    ],
                    Scalar::Opaque => vec![FilterOp::Equal, FilterOp::Is],
                    Scalar::JSON => vec![],   // unreachable, not in schema
                    Scalar::Cursor => vec![], // unreachable, not in schema
                };

                supported_ops
                    .iter()
                    .filter_map(|op| match op {
                        FilterOp::Equal
                        | FilterOp::NotEqual
                        | FilterOp::GreaterThan
                        | FilterOp::GreaterThanEqualTo
                        | FilterOp::LessThan
                        | FilterOp::LessThanEqualTo
                        | FilterOp::StartsWith
                        | FilterOp::Like
                        | FilterOp::ILike
                        | FilterOp::RegEx
                        | FilterOp::IRegEx => Some(__InputValue {
                            name_: op.to_string(),
                            type_: __Type::Scalar(scalar.clone()),
                            description: None,
                            default_value: None,
                            sql_type: None,
                        }),
                        FilterOp::In => Some(__InputValue {
                            name_: op.to_string(),
                            type_: __Type::List(ListType {
                                type_: Box::new(__Type::NonNull(NonNullType {
                                    type_: Box::new(__Type::Scalar(scalar.clone())),
                                })),
                            }),
                            description: None,
                            default_value: None,
                            sql_type: None,
                        }),
                        FilterOp::Is => Some(__InputValue {
                            name_: "is".to_string(),
                            type_: __Type::Enum(EnumType {
                                enum_: EnumSource::FilterIs,
                                schema: Arc::clone(&self.schema),
                            }),
                            description: None,
                            default_value: None,
                            sql_type: None,
                        }),
                        // shouldn't happen since we've covered all cases in supported_ops
                        FilterOp::Contains | FilterOp::ContainedBy | FilterOp::Overlap => None,
                    })
                    .collect()
            }
            FilterableType::Enum(enum_) => {
                vec![
                    __InputValue {
                        name_: "eq".to_string(),
                        type_: __Type::Enum(enum_.clone()),
                        description: None,
                        default_value: None,
                        sql_type: None,
                    },
                    __InputValue {
                        name_: "neq".to_string(),
                        type_: __Type::Enum(enum_.clone()),
                        description: None,
                        default_value: None,
                        sql_type: None,
                    },
                    __InputValue {
                        name_: "in".to_string(),
                        type_: __Type::List(ListType {
                            type_: Box::new(__Type::NonNull(NonNullType {
                                type_: Box::new(__Type::Enum(enum_.clone())),
                            })),
                        }),
                        description: None,
                        default_value: None,
                        sql_type: None,
                    },
                    __InputValue {
                        name_: "is".to_string(),
                        type_: __Type::Enum(EnumType {
                            enum_: EnumSource::FilterIs,
                            schema: Arc::clone(&self.schema),
                        }),
                        description: None,
                        default_value: None,
                        sql_type: None,
                    },
                ]
            }
            FilterableType::List(list_type) => {
                let supported_ops = [
                    FilterOp::Contains,
                    FilterOp::ContainedBy,
                    FilterOp::Equal,
                    FilterOp::Overlap,
                    FilterOp::Is,
                ];

                supported_ops
                    .iter()
                    .map(|op| match op {
                        FilterOp::Is => __InputValue {
                            name_: "is".to_string(),
                            type_: __Type::Enum(EnumType {
                                enum_: EnumSource::FilterIs,
                                schema: Arc::clone(&self.schema),
                            }),
                            description: None,
                            default_value: None,
                            sql_type: None,
                        },
                        _ => __InputValue {
                            name_: op.to_string(),
                            type_: __Type::List(ListType {
                                type_: Box::new(__Type::NonNull(NonNullType {
                                    type_: Box::new(*list_type.type_.clone()),
                                })),
                            }),
                            description: None,
                            default_value: None,
                            sql_type: None,
                        },
                    })
                    .collect()
            }
        };

        infields.sort_by_key(|a| a.name());
        Some(infields)
    }
}

pub(crate) const AND_FILTER_NAME: &str = "and";
pub(crate) const OR_FILTER_NAME: &str = "or";
pub(crate) const NOT_FILTER_NAME: &str = "not";

impl ___Type for FilterEntityType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::INPUT_OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}Filter",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        let mut and_column_exists = false;
        let mut or_column_exists = false;
        let mut not_column_exists = false;

        let mut f: Vec<__InputValue> = self
            .table
            .columns
            .iter()
            .filter(|x| x.permissions.is_selectable)
            // No filtering on composites
            .filter(|x| !self.schema.context.is_composite(x.type_oid))
            // No filtering on json/b. they do not support = or <>
            .filter(|x| !["json", "jsonb"].contains(&x.type_name.as_ref()))
            .filter_map(|col| {
                if let Some(utype) = sql_column_to_graphql_type(col, &self.schema) {
                    let column_graphql_name = self.schema.graphql_column_field_name(col);

                    if column_graphql_name == AND_FILTER_NAME {
                        and_column_exists = true;
                    }
                    if column_graphql_name == OR_FILTER_NAME {
                        or_column_exists = true;
                    }
                    if column_graphql_name == NOT_FILTER_NAME {
                        not_column_exists = true;
                    }

                    match utype.nullable_type() {
                        __Type::Scalar(s) => Some(__InputValue {
                            name_: column_graphql_name,
                            type_: __Type::FilterType(FilterTypeType {
                                entity: FilterableType::Scalar(s),
                                schema: Arc::clone(&self.schema),
                            }),
                            description: None,
                            default_value: None,
                            sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                        }),
                        __Type::Enum(e) => Some(__InputValue {
                            name_: column_graphql_name,
                            type_: __Type::FilterType(FilterTypeType {
                                entity: FilterableType::Enum(e),
                                schema: Arc::clone(&self.schema),
                            }),
                            description: None,
                            default_value: None,
                            sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                        }),
                        __Type::List(l) => match l.type_.nullable_type() {
                            // Only non-json scalars are supported in list types
                            __Type::Scalar(
                                Scalar::Int
                                | Scalar::Float
                                | Scalar::String(_)
                                | Scalar::Boolean
                                | Scalar::UUID
                                | Scalar::BigInt
                                | Scalar::BigFloat
                                | Scalar::Time
                                | Scalar::Date
                                | Scalar::Datetime,
                            ) => Some(__InputValue {
                                name_: column_graphql_name,
                                type_: __Type::FilterType(FilterTypeType {
                                    entity: FilterableType::List(l),
                                    schema: Arc::clone(&self.schema),
                                }),
                                description: None,
                                default_value: None,
                                sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                            }),
                            _ => None,
                        },
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .filter(|x| is_valid_graphql_name(&x.name_))
            .collect();

        if self.table.primary_key().is_some() {
            let pkey_cols = self
                .table
                .primary_key_columns()
                .into_iter()
                .cloned()
                .collect();

            f.push(__InputValue {
                name_: "nodeId".to_string(),
                type_: __Type::FilterType(FilterTypeType {
                    entity: FilterableType::Scalar(Scalar::ID),
                    schema: Arc::clone(&self.schema),
                }),
                description: None,
                default_value: None,
                sql_type: Some(NodeSQLType::NodeId(pkey_cols)),
            });
        }

        // If there is a column named `and` (and inflection is disabled) and
        // we were to add the `and` filter entry in this list there would be two
        // entries named `and` in the list returned by this method. Then
        // during filter argument validation both of these will be checked
        // against any key in the input named `and` and one of them will fail
        // the validation. This means the user can neither use a simple filter
        // on the `and` column nor use `and` in compound expressions.
        //
        // To prevent this we do not add the `and` filter entry which essentially
        // disables the `and` compound filter. But at least the user is able to
        // use simple column filters.
        //
        // Similiar logic applies for `or` and `not` filters.
        //
        // Arguably this smartness is unnecessary because users should not
        // name their columns `and`, `or` or `not` but the counter argument is
        // that in the case they do make such a mistake we degrade gracefully
        // instead of punishing them too harshly.
        if !and_column_exists {
            f.push(__InputValue {
                name_: AND_FILTER_NAME.to_string(),
                type_: __Type::List(ListType {
                    type_: Box::new(__Type::NonNull(NonNullType {
                        type_: Box::new(__Type::FilterEntity(FilterEntityType {
                            table: Arc::clone(&self.table),
                            schema: self.schema.clone(),
                        })),
                    })),
                }),
                description: Some(
                    "Returns true only if all its inner filters are true, otherwise returns false"
                        .to_string(),
                ),
                default_value: None,
                sql_type: None,
            });
        }
        if !or_column_exists {
            f.push(__InputValue {
                name_: OR_FILTER_NAME.to_string(),
                type_: __Type::List(ListType {
                    type_: Box::new(__Type::NonNull(NonNullType {
                        type_: Box::new( __Type::FilterEntity(FilterEntityType {
                            table: Arc::clone(&self.table),
                            schema: self.schema.clone(),
                        }))
                    })),
                }),
                description: Some(
                    "Returns true if at least one of its inner filters is true, otherwise returns false".to_string(),
                ),
                default_value: None,
                sql_type: None,
            });
        }
        if !not_column_exists {
            f.push(__InputValue {
                name_: NOT_FILTER_NAME.to_string(),
                type_: __Type::FilterEntity(FilterEntityType {
                    table: Arc::clone(&self.table),
                    schema: self.schema.clone(),
                }),
                description: Some("Negates a filter".to_string()),
                default_value: None,
                sql_type: None,
            });
        }

        Some(f)
    }
}

impl ___Type for OrderByType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::ENUM
    }

    fn name(&self) -> Option<String> {
        Some("OrderByDirection".to_string())
    }

    fn description(&self) -> Option<String> {
        Some("Defines a per-field sorting order".to_string())
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn enum_values(&self, _include_deprecated: bool) -> Option<Vec<__EnumValue>> {
        Some(vec![
            __EnumValue {
                name: "AscNullsFirst".to_string(),
                description: Some("Ascending order, nulls first".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "AscNullsLast".to_string(),
                description: Some("Ascending order, nulls last".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "DescNullsFirst".to_string(),
                description: Some("Descending order, nulls first".to_string()),
                deprecation_reason: None,
            },
            __EnumValue {
                name: "DescNullsLast".to_string(),
                description: Some("Descending order, nulls last".to_string()),
                deprecation_reason: None,
            },
        ])
    }
}

impl ___Type for OrderByEntityType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::INPUT_OBJECT
    }

    fn name(&self) -> Option<String> {
        Some(format!(
            "{}OrderBy",
            self.schema.graphql_table_base_type_name(&self.table)
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        None
    }

    fn input_fields(&self) -> Option<Vec<__InputValue>> {
        Some(
            self.table
                .columns
                .iter()
                .filter(|x| x.permissions.is_selectable)
                // No ordering by arrays
                .filter(|x| !x.type_name.ends_with("[]"))
                // No ordering by composites
                .filter(|x| !self.schema.context.is_composite(x.type_oid))
                // No ordering by json/b. they do not support = or <>
                .filter(|x| !["json", "jsonb"].contains(&x.type_name.as_ref()))
                .map(|col| __InputValue {
                    name_: self.schema.graphql_column_field_name(col),
                    type_: __Type::OrderBy(OrderByType {}),
                    description: None,
                    default_value: None,
                    sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                })
                .filter(|x| is_valid_graphql_name(&x.name_))
                .collect(),
        )
    }
}

#[derive(Serialize)]
pub struct ErrorMessage {
    pub message: String,
}

use super::omit::Omit;

#[derive(Serialize)]
pub struct GraphQLResponse {
    #[serde(skip_serializing_if = "Omit::is_omit")]
    pub data: Omit<serde_json::Value>,

    #[serde(skip_serializing_if = "Omit::is_omit")]
    pub errors: Omit<Vec<ErrorMessage>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct __Schema {
    pub context: Arc<Context>,
}

#[cached(
    type = "SizedCache<String, HashMap<String, __Type>>",
    create = "{ SizedCache::with_size(200) }",
    convert = r#"{ serde_json::ser::to_string(&schema.context.config).expect("schema config should be a string") }"#
)]
pub fn type_map(schema: &__Schema) -> HashMap<String, __Type> {
    let tmap: HashMap<String, __Type> = schema
        .types()
        .into_iter()
        .filter(|x| x.name().is_some())
        .map(|x| (x.name().expect("type should have a name"), x))
        .collect();
    tmap
}

impl __Schema {
    // types: [__Type!]!
    pub fn types(&self) -> Vec<__Type> {
        // This is is lightweight because context is Rc
        let schema_rc = Arc::new(self.clone());

        let mut types_: Vec<__Type> = vec![
            __Type::__TypeKind(__TypeKindType),
            __Type::__Schema(__SchemaType),
            __Type::__Type(__TypeType),
            __Type::__Field(__FieldType),
            __Type::__InputValue(__InputValueType),
            __Type::__EnumValue(__EnumValueType),
            __Type::__DirectiveLocation(__DirectiveLocationType),
            __Type::__Directive(__DirectiveType),
            __Type::PageInfo(PageInfoType),
            __Type::Scalar(Scalar::ID),
            __Type::Scalar(Scalar::Int),
            __Type::Scalar(Scalar::Float),
            __Type::Scalar(Scalar::String(None)),
            __Type::Scalar(Scalar::Boolean),
            __Type::Scalar(Scalar::Date),
            __Type::Scalar(Scalar::Time),
            __Type::Scalar(Scalar::Datetime),
            __Type::Scalar(Scalar::BigInt),
            __Type::Scalar(Scalar::UUID),
            __Type::Scalar(Scalar::JSON),
            __Type::Scalar(Scalar::Cursor),
            __Type::Scalar(Scalar::BigFloat),
            __Type::Scalar(Scalar::Opaque),
            __Type::Enum(EnumType {
                enum_: EnumSource::FilterIs,
                schema: Arc::clone(&schema_rc),
            }),
            __Type::OrderBy(OrderByType {}),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::ID),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::Int),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::Float),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::String(None)),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::Boolean),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::Date),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::Time),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::Datetime),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::BigInt),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::UUID),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::BigFloat),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::Scalar(Scalar::Opaque),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::Int)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::Float)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::String(None))),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::Date)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::Time)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::Datetime)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::BigInt)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::UUID)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::FilterType(FilterTypeType {
                entity: FilterableType::List(ListType {
                    type_: Box::new(__Type::Scalar(Scalar::BigFloat)),
                }),
                schema: Arc::clone(&schema_rc),
            }),
            __Type::Query(QueryType {
                schema: Arc::clone(&schema_rc),
            }),
            __Type::NodeInterface(NodeInterfaceType {
                schema: Arc::clone(&schema_rc),
            }),
        ];

        if self.mutations_exist() {
            types_.push(__Type::Mutation(MutationType {
                schema: Arc::clone(&schema_rc),
            }));
        }

        for table in self
            .context
            .tables
            .values()
            .filter(|x| self.graphql_table_select_types_are_valid(x))
        {
            types_.push(__Type::Node(NodeType {
                table: Arc::clone(table),
                fkey: None,
                reverse_reference: None,
                schema: Arc::clone(&schema_rc),
            }));
            types_.push(__Type::Edge(EdgeType {
                table: Arc::clone(table),
                schema: Arc::clone(&schema_rc),
            }));
            types_.push(__Type::Connection(ConnectionType {
                table: Arc::clone(table),
                fkey: None,
                schema: Arc::clone(&schema_rc),
            }));

            types_.push(__Type::FilterEntity(FilterEntityType {
                table: Arc::clone(table),
                schema: Arc::clone(&schema_rc),
            }));

            types_.push(__Type::OrderByEntity(OrderByEntityType {
                table: Arc::clone(table),
                schema: Arc::clone(&schema_rc),
            }));

            if self.graphql_table_insert_types_are_valid(table) {
                types_.push(__Type::InsertInput(InsertInputType {
                    table: Arc::clone(table),
                    schema: Arc::clone(&schema_rc),
                }));
                types_.push(__Type::InsertResponse(InsertResponseType {
                    table: Arc::clone(table),
                    schema: Arc::clone(&schema_rc),
                }));
            }

            if self.graphql_table_update_types_are_valid(table) {
                types_.push(__Type::UpdateInput(UpdateInputType {
                    table: Arc::clone(table),
                    schema: Arc::clone(&schema_rc),
                }));
                types_.push(__Type::UpdateResponse(UpdateResponseType {
                    table: Arc::clone(table),
                    schema: Arc::clone(&schema_rc),
                }));
            }

            if self.graphql_table_delete_types_are_valid(table) {
                types_.push(__Type::DeleteResponse(DeleteResponseType {
                    table: Arc::clone(table),
                    schema: Arc::clone(&schema_rc),
                }));
            }

            // Add Aggregate types if the table is selectable
            if self.graphql_table_select_types_are_valid(table) {
                // Only add aggregate types if the directive is enabled
                if let Some(aggregate_directive) = table.directives.aggregate.as_ref() {
                    if aggregate_directive.enabled {
                        types_.push(__Type::Aggregate(AggregateType {
                            table: Arc::clone(table),
                            schema: Arc::clone(&schema_rc),
                        }));
                        // Check if there are any columns aggregatable by sum/avg
                        if table
                            .columns
                            .iter()
                            .any(|c| is_aggregatable(c, &AggregateOperation::Sum))
                        {
                            types_.push(__Type::AggregateNumeric(AggregateNumericType {
                                table: Arc::clone(table),
                                schema: Arc::clone(&schema_rc),
                                aggregate_op: AggregateOperation::Sum,
                            }));
                            types_.push(__Type::AggregateNumeric(AggregateNumericType {
                                table: Arc::clone(table),
                                schema: Arc::clone(&schema_rc),
                                aggregate_op: AggregateOperation::Avg,
                            }));
                        }
                        // Check if there are any columns aggregatable by min/max
                        if table
                            .columns
                            .iter()
                            .any(|c| is_aggregatable(c, &AggregateOperation::Min))
                        {
                            types_.push(__Type::AggregateNumeric(AggregateNumericType {
                                table: Arc::clone(table),
                                schema: Arc::clone(&schema_rc),
                                aggregate_op: AggregateOperation::Min,
                            }));
                            types_.push(__Type::AggregateNumeric(AggregateNumericType {
                                table: Arc::clone(table),
                                schema: Arc::clone(&schema_rc),
                                aggregate_op: AggregateOperation::Max,
                            }));
                        }
                    }
                }
            }
        }

        for (_, enum_) in self
            .context
            .enums
            .iter()
            .filter(|(_, x)| x.permissions.is_usable)
            .filter(|(_, x)| self.context.schemas.contains_key(&x.schema_oid))
        {
            let enum_type = EnumType {
                enum_: EnumSource::Enum(Arc::clone(enum_)),
                schema: Arc::clone(&schema_rc),
            };

            let enum_filter = __Type::FilterType(FilterTypeType {
                entity: FilterableType::Enum(enum_type.clone()),
                schema: Arc::clone(&schema_rc),
            });

            types_.push(__Type::Enum(enum_type));
            types_.push(enum_filter);
        }

        types_.sort_by_key(|a| a.name());
        types_
    }

    pub fn mutations_exist(&self) -> bool {
        let mutation = MutationType {
            schema: Arc::new(self.clone()),
        };
        if let Some(fields) = mutation.fields(true) {
            if !fields.is_empty() {
                return true;
            }
        }
        false
    }

    // queryType: __Type!
    #[allow(dead_code)]
    pub fn query_type(&self) -> __Type {
        __Type::Query(QueryType {
            schema: Arc::new(self.clone()),
        })
    }

    // mutationType: __Type
    #[allow(dead_code)]
    pub fn mutation_type(&self) -> Option<__Type> {
        let mutation = MutationType {
            schema: Arc::new(self.clone()),
        };

        match mutation.fields(true).unwrap_or_default().len() {
            0 => None,
            _ => Some(__Type::Mutation(mutation)),
        }
    }

    // subscriptionType: __Type
    #[allow(dead_code)]
    pub fn subscription_type(&self) -> Option<__Type> {
        None
    }

    // directives: [__Directive!]!
    #[allow(dead_code)]
    pub fn directives(&self) -> Vec<__Directive> {
        vec![
            __Directive {
                name: "include".to_string(),
                description: Some(
                    "This field or fragment will be included only when the `if` argument is true."
                        .to_string(),
                ),
                locations: vec![
                    __DirectiveLocation::Field,
                    __DirectiveLocation::FragmentSpread,
                    __DirectiveLocation::InlineFragment,
                ],
                args: vec![__InputValue {
                    name_: "if".to_string(),
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                    }),
                    description: Some("Included when true".to_string()),
                    default_value: None,
                    sql_type: None,
                }],
                is_repeatable: false,
            },
            __Directive {
                name: "skip".to_string(),
                description: Some(
                    "This field or fragment will be skipped when the `if` argument is true."
                        .to_string(),
                ),
                locations: vec![
                    __DirectiveLocation::Field,
                    __DirectiveLocation::FragmentSpread,
                    __DirectiveLocation::InlineFragment,
                ],
                args: vec![__InputValue {
                    name_: "if".to_string(),
                    type_: __Type::NonNull(NonNullType {
                        type_: Box::new(__Type::Scalar(Scalar::Boolean)),
                    }),
                    description: Some("Skipped when true".to_string()),
                    default_value: None,
                    sql_type: None,
                }],
                is_repeatable: false,
            },
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct AggregateType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct AggregateNumericType {
    pub table: Arc<Table>,
    pub schema: Arc<__Schema>,
    pub aggregate_op: AggregateOperation,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum AggregateOperation {
    Sum,
    Avg,
    Min,
    Max,
    // Count is handled directly in AggregateType
}

impl AggregateOperation {
    // Helper for descriptive terms used in descriptions
    fn descriptive_term(&self) -> &str {
        match self {
            AggregateOperation::Sum => "summation",
            AggregateOperation::Avg => "average",
            AggregateOperation::Min => "minimum",
            AggregateOperation::Max => "maximum",
        }
    }

    // Helper for capitalized descriptive terms used in field descriptions
    fn capitalized_descriptive_term(&self) -> &str {
        match self {
            AggregateOperation::Sum => "Sum",
            AggregateOperation::Avg => "Average",
            AggregateOperation::Min => "Minimum",
            AggregateOperation::Max => "Maximum",
        }
    }
}

impl std::fmt::Display for AggregateOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregateOperation::Sum => write!(f, "Sum"),
            AggregateOperation::Avg => write!(f, "Avg"), // GraphQL schema uses "Avg" for the type name part
            AggregateOperation::Min => write!(f, "Min"),
            AggregateOperation::Max => write!(f, "Max"),
        }
    }
}

/// Determines if a column's type is suitable for a given aggregate operation.
fn is_aggregatable(column: &Column, op: &AggregateOperation) -> bool {
    let Some(ref type_) = column.type_ else {
        return false;
    };

    // Removed duplicated closures, will use helper functions below

    match op {
        // Sum/Avg only make sense for numeric types
        AggregateOperation::Sum | AggregateOperation::Avg => {
            // Check category first for arrays/enums, then check name for base types
            match type_.category {
                TypeCategory::Other => is_pg_numeric_type(&type_.name),
                _ => false, // Only allow sum/avg on base numeric types for now
            }
        }
        // Min/Max can work on more types (numeric, string, date/time, etc.)
        AggregateOperation::Min | AggregateOperation::Max => {
            match type_.category {
                TypeCategory::Other => {
                    is_pg_numeric_type(&type_.name)
                        || is_pg_string_type(&type_.name)
                        || is_pg_datetime_type(&type_.name)
                        || is_pg_boolean_type(&type_.name)
                }
                _ => false, // Don't allow min/max on composites, arrays, tables, pseudo
            }
        }
    }
}

/// Returns the appropriate GraphQL scalar type for an aggregate result.
fn aggregate_result_type(column: &Column, op: &AggregateOperation) -> Option<Scalar> {
    let type_ = column.type_.as_ref()?;

    match op {
        AggregateOperation::Sum => {
            // SUM of integers often results in bigint
            // SUM of float/numeric results in bigfloat
            // Let's simplify and return BigInt for int-like, BigFloat otherwise
            if is_pg_small_integer_type(&type_.name) {
                Some(Scalar::BigInt)
            } else if is_pg_numeric_type(&type_.name) {
                Some(Scalar::BigFloat)
            } else {
                None
            }
        }
        AggregateOperation::Avg => {
            if is_pg_numeric_type(&type_.name) {
                Some(Scalar::BigFloat)
            } else {
                None
            }
        }
        AggregateOperation::Min | AggregateOperation::Max => {
            if is_pg_numeric_type(&type_.name) {
                sql_type_to_scalar(&type_.name, column.max_characters)
            } else if is_pg_string_type(&type_.name) {
                Some(Scalar::String(column.max_characters))
            } else if is_pg_datetime_type(&type_.name) {
                sql_type_to_scalar(&type_.name, column.max_characters)
            } else if is_pg_boolean_type(&type_.name) {
                Some(Scalar::Boolean)
            } else {
                None
            }
        }
    }
}

impl ___Type for AggregateType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        let table_base_type_name = &self.schema.graphql_table_base_type_name(&self.table);
        Some(format!("{table_base_type_name}Aggregate"))
    }

    fn description(&self) -> Option<String> {
        let table_base_type_name = &self.schema.graphql_table_base_type_name(&self.table);
        Some(format!("Aggregate results for `{table_base_type_name}`"))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        let mut fields = Vec::new();

        // Count field (always present)
        fields.push(__Field {
            name_: "count".to_string(),
            type_: __Type::NonNull(NonNullType {
                type_: Box::new(__Type::Scalar(Scalar::Int)),
            }),
            args: vec![],
            description: Some("The number of records matching the query".to_string()),
            deprecation_reason: None,
            sql_type: None,
        });

        // Add fields for Sum, Avg, Min, Max if there are any aggregatable columns
        let has_sum_avgable = self
            .table
            .columns
            .iter()
            .any(|c| is_aggregatable(c, &AggregateOperation::Sum));
        let has_min_maxable = self
            .table
            .columns
            .iter()
            .any(|c| is_aggregatable(c, &AggregateOperation::Min));

        if has_sum_avgable {
            fields.push(__Field {
                name_: "sum".to_string(),
                type_: __Type::AggregateNumeric(AggregateNumericType {
                    table: Arc::clone(&self.table),
                    schema: Arc::clone(&self.schema),
                    aggregate_op: AggregateOperation::Sum,
                }),
                args: vec![],
                description: Some("Summation aggregates for numeric fields".to_string()),
                deprecation_reason: None,
                sql_type: None,
            });
            fields.push(__Field {
                name_: "avg".to_string(),
                type_: __Type::AggregateNumeric(AggregateNumericType {
                    table: Arc::clone(&self.table),
                    schema: Arc::clone(&self.schema),
                    aggregate_op: AggregateOperation::Avg,
                }),
                args: vec![],
                description: Some("Average aggregates for numeric fields".to_string()),
                deprecation_reason: None,
                sql_type: None,
            });
        }

        if has_min_maxable {
            fields.push(__Field {
                name_: "min".to_string(),
                type_: __Type::AggregateNumeric(AggregateNumericType {
                    table: Arc::clone(&self.table),
                    schema: Arc::clone(&self.schema),
                    aggregate_op: AggregateOperation::Min,
                }),
                args: vec![],
                description: Some("Minimum aggregates for comparable fields".to_string()),
                deprecation_reason: None,
                sql_type: None,
            });
            fields.push(__Field {
                name_: "max".to_string(),
                type_: __Type::AggregateNumeric(AggregateNumericType {
                    table: Arc::clone(&self.table),
                    schema: Arc::clone(&self.schema),
                    aggregate_op: AggregateOperation::Max,
                }),
                args: vec![],
                description: Some("Maximum aggregates for comparable fields".to_string()),
                deprecation_reason: None,
                sql_type: None,
            });
        }
        Some(fields)
    }
}

impl ___Type for AggregateNumericType {
    fn kind(&self) -> __TypeKind {
        __TypeKind::OBJECT
    }

    fn name(&self) -> Option<String> {
        let table_base_type_name = &self.schema.graphql_table_base_type_name(&self.table);
        // Use Display trait for op_name
        Some(format!(
            "{table_base_type_name}{}AggregateResult",
            self.aggregate_op
        ))
    }

    fn description(&self) -> Option<String> {
        let table_base_type_name = &self.schema.graphql_table_base_type_name(&self.table);
        Some(format!(
            "Result of {} aggregation for `{table_base_type_name}`",
            self.aggregate_op.descriptive_term()
        ))
    }

    fn fields(&self, _include_deprecated: bool) -> Option<Vec<__Field>> {
        let mut fields = Vec::new();

        for col in self.table.columns.iter() {
            if is_aggregatable(col, &self.aggregate_op) {
                if let Some(scalar_type) = aggregate_result_type(col, &self.aggregate_op) {
                    let field_name = self.schema.graphql_column_field_name(col);
                    fields.push(__Field {
                        name_: field_name.clone(),
                        type_: __Type::Scalar(scalar_type),
                        args: vec![],
                        description: Some(format!(
                            "{} of {} across all matching records",
                            self.aggregate_op.capitalized_descriptive_term(),
                            field_name
                        )),
                        deprecation_reason: None,
                        sql_type: Some(NodeSQLType::Column(Arc::clone(col))),
                    });
                }
            }
        }
        if fields.is_empty() {
            None
        } else {
            Some(fields)
        }
    }
}

// Converts SQL type name to a GraphQL Scalar, needed for aggregate_result_type
// This function might already exist or needs to be created/adapted.
// Placeholder implementation:
fn sql_type_to_scalar(sql_type_name: &str, typmod: Option<i32>) -> Option<Scalar> {
    // Simplified mapping - adapt based on existing logic in sql_types.rs or elsewhere
    match sql_type_name {
        "int2" | "int4" => Some(Scalar::Int),
        "int8" => Some(Scalar::BigInt),
        "float4" | "float8" | "numeric" | "decimal" => Some(Scalar::BigFloat), // Use BigFloat for precision
        "text" | "varchar" | "char" | "bpchar" | "name" => Some(Scalar::String(typmod)),
        "bool" => Some(Scalar::Boolean),
        "date" => Some(Scalar::Date),
        "time" | "timetz" => Some(Scalar::Time),
        "timestamp" | "timestamptz" => Some(Scalar::Datetime),
        "uuid" => Some(Scalar::UUID),
        "json" | "jsonb" => Some(Scalar::JSON),
        _ => Some(Scalar::Opaque), // Fallback for unknown types
    }
}

// Helper functions for PostgreSQL type checking (extracted to deduplicate)
fn is_pg_numeric_type(name: &str) -> bool {
    matches!(
        name,
        "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" | "decimal" | "money"
    )
}

fn is_pg_string_type(name: &str) -> bool {
    matches!(
        name,
        "text" | "varchar" | "char" | "bpchar" | "name" | "citext"
    )
}

fn is_pg_datetime_type(name: &str) -> bool {
    matches!(
        name,
        "date" | "time" | "timetz" | "timestamp" | "timestamptz"
    )
}

fn is_pg_boolean_type(name: &str) -> bool {
    matches!(name, "bool")
}

fn is_pg_small_integer_type(name: &str) -> bool {
    matches!(name, "int2" | "int4" | "int8")
}
