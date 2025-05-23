use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;
use std::mem;
use std::ops::Add;
use std::sync::{Arc, LazyLock};

use super::error::{input_error, resolver_error, schema_error, GqlError};
use super::ext::IntoExt;
use super::schema::{gql_to_sql_kind, sql_value_to_gql_value};
use crate::dbs::Session;
use crate::fnc::time::format;
use crate::gql::error::internal_error;
use crate::gql::ext::TryAsExt;
use crate::gql::schema::{kind_to_type, unwrap_type};
use crate::gql::utils::{field_val_erase_owned, ErasedRecord, GQLTx, GqlValueUtils};
use crate::kvs::{Datastore, Transaction};
use crate::sql::order::{OrderList, Ordering};
use crate::sql::statements::{DefineFieldStatement, DefineTableStatement, SelectStatement};
use crate::sql::{self, Ident, Literal, Part, Table, TableType};
use crate::sql::{Cond, Fields};
use crate::sql::{Expression, Value as SqlValue};
use crate::sql::{Idiom, Kind};
use crate::sql::{Statement, Thing};
use async_graphql::dynamic::indexmap::IndexMap;
use async_graphql::dynamic::TypeRef;
use async_graphql::dynamic::{Enum, FieldValue, Type};
use async_graphql::dynamic::{EnumItem, FieldFuture};
use async_graphql::dynamic::{Field, ResolverContext};
use async_graphql::dynamic::{InputObject, Object};
use async_graphql::dynamic::{InputValue, Union};
use async_graphql::types::connection::{Connection, Edge, PageInfo};
use async_graphql::Name;
use async_graphql::Value as GqlValue;
use inflector::Inflector;
use log::trace;
// macro_rules! order {
// 	(asc, $field:expr) => {{
// 		let mut tmp = sql::Order::default();
// 		tmp.value = $field.into();
// 		tmp.direction = true;
// 		tmp
// 	}};
// 	(desc, $field:expr) => {{
// 		let mut tmp = sql::Order::default();
// 		tmp.value = $field.into();
// 		tmp
// 	}};
// }
macro_rules! first_input {
	() => {
		InputValue::new("first", TypeRef::named(TypeRef::INT))
        .description("Returns the first *n* elements from the list.")
	};
}

macro_rules! last_input {
	() => {
		InputValue::new("last", TypeRef::named(TypeRef::INT))
        .description("Returns the last *n* elements from the list.")
	};
}

macro_rules! before_input {
	() => {
		InputValue::new("before", TypeRef::named(TypeRef::STRING))
        .description("Returns the elements in the list that come before the specified cursor.")
	};
}

macro_rules! after_input {
	() => {
		InputValue::new("after", TypeRef::named(TypeRef::STRING))
        .description("Returns the elements in the list that come after the specified cursor.")
	};
}

macro_rules! limit_input {
	() => {
		InputValue::new("limit", TypeRef::named(TypeRef::INT))
        .description("xxx")
	};
}

macro_rules! id_input {
	() => {
		InputValue::new("id", TypeRef::named(TypeRef::ID))
	};
}

/// This macro needs the order input types to be defined with `define_order_input_types`.
macro_rules! order_input {
	($name: expr) => {
		InputValue::new("orderBy", TypeRef::named(format!("{}Order", $name.to_pascal_case())))
        .description(format!("Ordering options for `{}` connections.", $name))
	};
}

macro_rules! filter_input {
	($name: expr) => {
		InputValue::new("filterBy", TypeRef::named(format!("{}Filter", $name.to_pascal_case())))
	};
}

macro_rules! define_page_info_type {
    ($types:ident) => {
        $types.push(Type::Object({
            Object::new("PageInfo")
            .field(
                Field::new(
                "hasNextPage",
                TypeRef::named_nn(TypeRef::BOOLEAN),
                page_info_resolver("".to_string(), None),
                ).description("When paginating forwards, are there more items?")
            )
            .field(
                Field::new(
                "hasPreviousPage",
                TypeRef::named_nn(TypeRef::BOOLEAN),
                page_info_resolver("".to_string(), None),
                ).description("When paginating backwards, are there more items?")
            )
            .field(
                Field::new(
                "startCursor",
                TypeRef::named(TypeRef::STRING),
                page_info_resolver("".to_string(), None),
                ).description("When paginating backwards, the cursor to continue.")
            )
            .field(
                Field::new(
                "endCursor",
                TypeRef::named(TypeRef::STRING),
                page_info_resolver("".to_string(), None),
                ).description("When paginating forwards, the cursor to continue.")
            )
            .description("Information about pagination in a connection.")
        }))
    };
}

macro_rules! define_order_direction_enum {
    ($types:ident) => {
        $types.push(Type::Enum({
            Enum::new("OrderDirection")
            .item(EnumItem::new("ASC").description("Specifies an ascending order for a given \
            `orderBy` argument."))
            .item(EnumItem::new("DESC").description("Specifies a descending order for a given \
            `orderBy` argument."))
            .description("Possible directions in which to order a list of \
            items when provided and `orderBy` argument.")
        }))
    };
}

/// This macro needs the order direction enum type defined. you may use
/// `define_order_direction_enum` for it.
macro_rules! define_order_input_types {
    (
        $types:ident,
        $base_name:expr,
        $( $field_enum_name:ident ),* $(,)?
    ) => {
        let base_name_pascal = $base_name.to_pascal_case();
        let enum_name = format!("{}OrderField", base_name_pascal);
        let obj_name = format!("{}Order", base_name_pascal);

        let order_by_enum = Enum::new(&enum_name)
            .item(EnumItem::new("ID").description(format!("{} by ID.", $base_name)))
            $(.item(EnumItem::new(stringify!($field_enum_name).to_screaming_snake_case())
                .description(format!("{} by {}.",
                $base_name, stringify!($field_enum_name).to_screaming_snake_case()))))*
            .description(format!("Properties by which {} can be ordered.", $base_name));
        $types.push(Type::Enum(order_by_enum));

        let order_by_obj = InputObject::new(&obj_name)
            .field(
                InputValue::new("field", TypeRef::named(&enum_name))
                .description(format!("The field to order {} by.", $base_name)))
            .field(
                InputValue::new("direction", TypeRef::named("OrderDirection"))
                .description("The ordering direction."))
            .description(format!("Ordering options for {} connections", $base_name));
        $types.push(Type::InputObject(order_by_obj))
    };
}

/// Adds a connection field to the specified object.
///
/// # Parameters
/// - (`obj`: The object to which the connection field is added.)
/// - `types`: The types vector to which the connection and edge types are added.
/// - `fd_name`: The name of the connection field.
/// - `node_ty_name`: The name of the node type.
/// - `connection_resolver`: The resolver for the connection field.
/// - `edges`: Additional edge fields.
/// - `args`: Additional connection arguments.
#[macro_export]
macro_rules! cursor_pagination {
    (
        $types:ident,
        $fd_name:expr,
        $node_ty_name:expr,
        //TODO
        // $connection_resolver:expr,      // The actual resolver for the connection field on $obj
        edge_fields: $edge_fields_expr:expr,
        args: [ $( $extra_connection_arg:expr ),* $(,)? ]
    ) => {
        {
            let mut edge = Object::new(format!("{}Edge", $node_ty_name))
                .field(Field::new(
                    "cursor",
                    TypeRef::named_nn(TypeRef::STRING),
                    page_info_resolver("".to_string(), None),
                ).description("A cursor for use in pagination."))
                .field(Field::new(
                    "node",
                    TypeRef::named($node_ty_name),
                    page_info_resolver("".to_string(), None),
                ).description("The item at the end of the edge."))
                .description("An edge in a connection.");
            for fd in $edge_fields_expr {
                edge = edge.field(fd);
            }

            let connection = Object::new(format!("{}Connection", $node_ty_name))
                .field(Field::new(
                    "edges",
                    TypeRef::named_list(format!("{}Edge", $node_ty_name)),
                    page_info_resolver("".to_string(), None),
                ).description("A list of edges."))
                .field(Field::new(
                    "nodes",
                    TypeRef::named_list($node_ty_name),
                    page_info_resolver("".to_string(), None),
                ).description("A list of nodes."))
                .field(Field::new(
                    "pageInfo",
                    TypeRef::named_nn("PageInfo"),
                    page_info_resolver("".to_string(), None),
                ).description("Information to aid in pagination."))
                .field(Field::new(
                    "totalCount",
                    TypeRef::named_nn(TypeRef::INT),
                    page_info_resolver("".to_string(), None),
                ).description("Identifies the total count of items in the connection."))
                .description(format!("The connection type for {}.", $node_ty_name));

            $types.push(Type::Object(edge));
            $types.push(Type::Object(connection));

            Field::new(
                $fd_name,
                TypeRef::named_nn(format!("{}Connection", $node_ty_name)),
                page_info_resolver("".to_string(), None),
            )
            .description(format!("The connection object for the table `{}`", $fd_name))
            .argument(after_input!())
            .argument(before_input!())
            .argument(first_input!())
            .argument(last_input!())
            $(.argument($extra_connection_arg))*
        }
    };
}

/// This macro is used to parse a field definition and add it to the object map.
/// It handles different kinds of fields, including nested fields and array fields.
/// It also manages the creation of connection fields for array types.
///
/// # Parameters
/// - `$fd`: The field definition to parse.
/// - `$types`: The types vector to which the field type is added.
/// - `$cursor`: A boolean indicating whether to use cursor pagination.
/// - `$tb_name`: The name of the table.
/// - `$map`: The object map to which the field is added.
/// - `$field_ident`: The identifier for the field.
/// - `$action_tokens`: The action tokens to execute after parsing the field.
macro_rules! parse_field {
    (
        $fd:ident,
        $types:ident,
        $cursor:ident,
        $tb_name:ident,
        $map:ident,
        |$field_ident:ident| $($action_tokens:tt)*
    ) => {
        let kind = match $fd.kind.clone() {
            Some(k) => k,
            None => continue
        };
        let kind_non_optional = kind.non_optional().clone();

        let parts: Vec<&Ident> = $fd.name.0.iter().filter_map(|part| match part {
            Part::Field(ident) => Some(ident),
            _ => None
        }).collect();

        // Should always contain at least the field name
        if parts.is_empty() { continue; }

        let fd_name = parts.as_slice().last().unwrap().to_string();
        let fd_name_gql = fd_name.to_camel_case();

        let fd_path = $fd.name.to_path()
            .replace("/", ".")
            .strip_prefix(".")
            .unwrap()
            .to_string();
        let fd_path_parent = remove_last_segment(&*fd_path.as_str());

        // Use table name for e.g., object uniqueness across multiple tables //TODO: not needed
        // for rel fields i think
        let mut path = Vec::with_capacity(parts.len() + 1);
        let table_ident = Ident::from($tb_name.clone());
        path.push(&table_ident);
        path.extend_from_slice(parts.as_slice());

        let fd_ty = kind_to_type(kind.clone(), $types, path.as_slice())?;

        // object map used to add fields step by step to the objects
        if kind_non_optional == Kind::Object {
            $map.insert(
                fd_path.clone(),
                Object::new(fd_ty.type_name())
                    .description(if let Some(ref c) = $fd.comment {
                        format!("{c}")
                    } else {
                        "".to_string()
                    }),
            );
        }

        if fd_path_parent.is_empty() { // top level field
            match kind_non_optional {
                // cursor connections only if specified in config
                Kind::Array(_, _) if $cursor => {
                    if let kind = kind.inner_kind().unwrap() {
                        let ty_ref = kind_to_type(kind.clone(), $types, path.as_slice())?;
                        let ty_name = ty_ref.type_name();

                        let $field_ident = cursor_pagination!($types, &fd_name_gql, ty_name,
                        edge_fields: [], args: []);
                        $($action_tokens)*;
                    }
                }
                _ => {
                     let $field_ident = Field::new(
                            fd_name_gql,
                            fd_ty,
                            make_table_field_resolver(fd_path.as_str(), $fd.kind.clone()),
                         // hier der resolver muss handlen koennen simple fields and
                         // arbitrary nested objects
                        )
                        .description(if let Some(ref c) = $fd.comment {
                            format!("{c}")
                        } else {
                            "".to_string()
                        });
                    $($action_tokens)*;
                }
            }
        } else { // nested field
            // Array inner type is scalar, thus already set when adding the list field
            if fd_path.chars().last() == Some('*') { continue; }

            // expects the parent's `DefineFieldStatement` to come before its children as is
            // with `tx.all_tb_fields()`
            match $map.remove(&fd_path_parent) {
                Some(obj) => {
                    $map.insert(fd_path_parent.clone(), Object::from(obj)
                        .field(Field::new(
                            fd_name_gql,
                            fd_ty,
                            make_table_field_resolver(fd_path.as_str(), $fd.kind.clone()),
                        ))
                        .description(if let Some(ref c) = $fd.comment {
                            format!("{c}")
                        } else {
                            "".to_string()
                        }),
                    );
                }
                None => return Err(internal_error("Nested field should have parent object.")),
            }
        }
    };
}


fn filter_name_from_table(tb_name: impl Display) -> String {
    // format!("Filter{}", tb_name.to_string().to_sentence_case())
    format!("{}FilterInput", tb_name.to_string().to_pascal_case())
}


fn remove_last_segment(input: &str) -> String {
    let mut parts = input.rsplitn(2, '.'); // Split from the right, limit to 2 parts
    parts.next(); // Discard the last segment
    parts.next().unwrap_or("").to_string() // Take the remaining part
}

fn remove_leading_dot(input: &str) -> &str {
    input.strip_prefix('.').unwrap_or(input)
}

#[allow(clippy::too_many_arguments)]
pub async fn process_tbs(
    tbs: Arc<[DefineTableStatement]>,
    mut query: Object,
    types: &mut Vec<Type>,
    tx: &Transaction,
    ns: &str,
    db: &str,
    session: &Session,
    datastore: &Arc<Datastore>,
    cursor: bool,
) -> Result<Object, GqlError> {
    // Type::Any is not supported. FIXME: throw error in the future.
    let (tables, relations): (Vec<&DefineTableStatement>, Vec<&DefineTableStatement>) = tbs
        .iter().partition(|tb| {
        match tb.kind {
            TableType::Normal => true,
            TableType::Relation(_) => false,
            TableType::Any => false,
        }
    });

    // trace!("tables: {:?}", tables);
    // trace!("relations: {:?}", relations);

    for tb in tables.iter() {
        let tb_name = tb.name.to_string();
        let first_tb_name = tb_name.clone();
        let second_tb_name = tb_name.clone();
        let tb_name_gql = tb_name.to_pascal_case();
        let tb_name_query = tb_name.to_camel_case(); // field name for the table in the query

        let mut gql_objects: BTreeMap<String, Object> = BTreeMap::new();

        let fds = tx.all_tb_fields(ns, db, &tb.name.0, None).await?;

        let mut tb_ty_obj = Object::new(tb_name_gql.clone())
            .field(Field::new(
                "id",
                TypeRef::named_nn(TypeRef::ID),
                make_table_field_resolver(
                    "id",
                    Some(Kind::Record(vec![Table::from(tb_name.clone())])),
                ),
            ))
            .implement("Record");

        // =======================================================
        // Parse Fields
        // =======================================================
        for fd in fds.iter() {
            // We have already defined "id", so we don't take any new definition for it.
            if fd.name.is_id() { continue; };

            parse_field!(fd, types, cursor, tb_name, gql_objects, |fd| tb_ty_obj = tb_ty_obj
                .field(fd));
        }

        // =======================================================
        // Add filters
        // =======================================================

        // Add additional orderBy fields here:
        define_order_input_types!(types, tb_name,);

        // =======================================================
        // Add single instance query
        // =======================================================

        let sess1 = session.to_owned();
        let kvs1 = datastore.clone();
        let fds1 = fds.clone();

        query = query.field(
            Field::new(
                tb_name_query.to_singular(),
                TypeRef::named(&tb_name_gql),
                move |ctx| {
                    let tb_name = first_tb_name.clone();
                    let kvs1 = kvs1.clone();
                    FieldFuture::new({
                        let sess1 = sess1.clone();
                        async move {
                            let gtx = GQLTx::new(&kvs1, &sess1).await?;

                            let args = ctx.args.as_index_map();
                            let id = match args.get("id").and_then(GqlValueUtils::as_string) {
                                Some(i) => i,
                                None => {
                                    return Err(input_error(
                                        "Schema validation failed: No id found in arguments",
                                    )
                                        .into());
                                }
                            };
                            let thing = match id.clone().try_into() {
                                Ok(t) => t,
                                Err(_) => Thing::from((tb_name, id)),
                            };

                            match gtx.get_record_field(thing, "id").await? {
                                SqlValue::Thing(t) => {
                                    let erased: ErasedRecord = (gtx, t);
                                    Ok(Some(field_val_erase_owned(erased)))
                                }
                                _ => Ok(None),
                            }
                        }
                    })
                },
            )
                .description(if let Some(ref c) = &tb.comment {
                    format!("{c}")
                } else {
                    format!("Generated from table `{}`\nallows querying a single record in a table by ID", &tb_name)
                })
                .argument(id_input!()),
        );

        // =======================================================
        // Add all instances query
        // =======================================================

        let sess2 = session.to_owned();
        let kvs2 = datastore.clone();
        let fds2 = fds.clone();

        if cursor {
            query = query.field(
                cursor_pagination!(
                types,
                tb_name_query.to_plural(),
                &tb_name_gql,
                edge_fields: [],
                args: [
                    order_input!(&tb_name)
                ]
            ));
            define_page_info_type!(types);
        } else {
            query = query.field(
                Field::new(
                    tb_name_query.to_plural(),
                    TypeRef::named_nn_list_nn(&tb_name_gql),
                    move |ctx| {
                        let tb_name = second_tb_name.clone();
                        let sess2 = sess2.clone();
                        let fds2 = fds.clone();
                        let kvs2 = kvs2.clone();
                        FieldFuture::new(async move {
                            let gtx = GQLTx::new(&kvs2, &sess2).await?;

                            let args = ctx.args.as_index_map();
                            trace!("received request with args: {args:?}");

                            // let start = args.get("start").and_then(|v| v.as_i64()).map(|s| s.intox());
                            //
                            // let limit = args.get("limit").and_then(|v| v.as_i64()).map(|l| l.intox());
                            //
                            // let order = args.get("order");
                            //
                            // let filter = args.get("filter");

                            // let orders = match order {
                            //     Some(GqlValue::Object(o)) => {
                            //         let mut orders = vec![];
                            //         let mut current = o;
                            //         loop {
                            //             let asc = current.get("asc");
                            //             let desc = current.get("desc");
                            //             match (asc, desc) {
                            //                 (Some(_), Some(_)) => {
                            //                     return Err("Found both ASC and DESC in order".into());
                            //                 }
                            //                 (Some(GqlValue::Enum(a)), None) => {
                            //                     orders.push(order!(asc, a.as_str()))
                            //                 }
                            //                 (None, Some(GqlValue::Enum(d))) => {
                            //                     orders.push(order!(desc, d.as_str()))
                            //                 }
                            //                 (_, _) => {
                            //                     break;
                            //                 }
                            //             }
                            //             if let Some(GqlValue::Object(next)) = current.get("then") {
                            //                 current = next;
                            //             } else {
                            //                 break;
                            //             }
                            //         }
                            //         Some(orders)
                            //     }
                            //     _ => None,
                            // };
                            // trace!("parsed orders: {orders:?}");

                            // let cond = match filter {
                            //     Some(f) => {
                            //         let o = match f {
                            //             GqlValue::Object(o) => o,
                            //             f => {
                            //                 error!("Found filter {f}, which should be object and should have been rejected by async graphql.");
                            //                 return Err("Value in cond doesn't fit schema".into());
                            //             }
                            //         };
                            //
                            //         let cond = cond_from_filter(o, &fds2)?;
                            //
                            //         Some(cond)
                            //     }
                            //     None => None,
                            // };
                            // trace!("parsed filter: {cond:?}");

                            // SELECT VALUE id FROM ...
                            let ast = Statement::Select({
                                SelectStatement {
                                    what: vec![SqlValue::Table(tb_name.intox())].into(),
                                    expr: Fields(
                                        vec![sql::Field::Single {
                                            expr: SqlValue::Idiom(Idiom::from("id")),
                                            alias: None,
                                        }],
                                        // this means the `value` keyword
                                        true,
                                    ),
                                    // order: orders.map(|x| Ordering::Order(OrderList(x))),
                                    // cond,
                                    // limit,
                                    // start,
                                    ..Default::default()
                                }
                            });
                            trace!("generated query ast: {ast:?}");

                            let res = gtx.process_stmt(ast).await?;

                            trace!("query result: {res:?}");

                            let res_vec =
                                match res {
                                    SqlValue::Array(a) => a,
                                    v => {
                                        error!("Found top level value, in result which should be array: {v:?}");
                                        return Err("Internal Error".into());
                                    }
                                };

                            trace!("query result array: {res_vec:?}");

                            let out: Result<Vec<FieldValue>, SqlValue> = res_vec
                                .0
                                .into_iter()
                                .map(|v| {
                                    v.try_as_thing().map(|t| {
                                        let erased: ErasedRecord = (gtx.clone(), t);
                                        field_val_erase_owned(erased)
                                    })
                                })
                                .collect();

                            match out {
                                Ok(l) => Ok(Some(FieldValue::list(l))),
                                Err(v) => {
                                    Err(internal_error(format!("expected thing, found: {v:?}")).into())
                                }
                            }
                        })
                    },
                )
                    .description(if let Some(ref c) = &tb.comment {
                        format!("{c}")
                    } else {
                        format!("Generated from table `{}`\nallows querying a table with filters",
                                &tb_name)
                    })
                    .argument(limit_input!())
                    .argument(order_input!(&tb_name))
                // .argument(filter_input!(&tb_name))
            );
        }

        // =======================================================
        // Add relations
        // =======================================================

        for rel in relations.iter().filter(|stmt| {
            match &stmt.kind {
                TableType::Relation(r) => match &r.from {
                    Some(Kind::Record(tbs)) => tbs.contains(&Table::from(tb_name.clone())),
                    _ => false,
                },
                _ => false,
            }
        }) {
            let rel_name = rel.name.to_string();

            let (ins, outs) = match &rel.kind {
                TableType::Relation(r) => match (&r.from, &r.to) {
                    (Some(Kind::Record(from)), Some(Kind::Record(to))) => (from, to),
                    _ => continue,
                },
                _ => continue,
            };

            let mut fd_map: BTreeMap<String, Object> = BTreeMap::new();
            let mut fd_vec = Vec::<Field>::new();

            let fds = tx.all_tb_fields(ns, db, &rel.name.0, None).await?;

            //todo?: das hier nur n mal machen. Also nur dann wenn nicht vec ins > 1, bzw schon in map
            // possible performance improvements by skipping fields for prev relations
            for fd in fds.iter().filter(|fd| {
                match fd.name.to_string().as_str() {
                    "in" => false,
                    "out" => false,
                    // "id" => false, // FIXME: prob not wanted
                    _ => true,
                }
            }) {
                parse_field!(fd, types, cursor, rel_name, fd_map, |fd| fd_vec.push(fd));
            }

            // Node type for the relation connection
            let node_ty_name = match outs.len() {
                // we have only one `to` table, thus we can use the object type directly
                1 => outs.first().unwrap().to_string().to_pascal_case(),
                // we have more than one `to` table, thus we need a union type
                _ => {
                    let mut tmp_union = Union::new(format!("{}Union", rel.name.to_raw().to_pascal_case()));
                    for n in outs {
                        tmp_union = tmp_union.possible_type(n.0.to_string().to_pascal_case());
                    }
                    // async_graphql types do not implement clone, thus we need to get the typename
                    // before the move
                    let union_name = tmp_union.type_name().to_string();
                    types.push(Type::Union(tmp_union));

                    union_name
                }
            };

            tb_ty_obj = tb_ty_obj.field(
                cursor_pagination!(
                types,
                rel.name.to_raw().to_camel_case().to_plural(),
                &node_ty_name,
                edge_fields: fd_vec,
                args: [
                    order_input!(&tb_name)
                ]
            ));

            define_order_input_types!(types, rel.name.to_raw(),);

            for (_, obj) in fd_map {
                types.push(Type::Object(obj));
            }
        }

        // =======================================================
        // Add types
        // =======================================================
        // for loop because Type::Object needs owned obj, not a reference
        for (_, obj) in gql_objects {
            types.push(Type::Object(obj));
        }
        types.push(Type::Object(tb_ty_obj));

        define_order_direction_enum!(types); // Needed for order_input
    }

    Ok(query)
}

//TODO: bug: type HomeTypeEnum enum is optional even though it shouldn't

fn make_table_field_resolver(
    // fd_name: impl Into<String>,
    fd_path: impl Into<String>,
    kind: Option<Kind>,
) -> impl for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync + 'static {
    let fd_path = fd_path.into();
    move |ctx: ResolverContext| {
        let fd_path = fd_path.clone();
        let field_kind = kind.clone();

        FieldFuture::new({
            async move {
                trace!(
                    "Creating/Running resolver for DB path '{}' (Kind: {:?}) with parent: {:?}",
                    fd_path,
                    field_kind,
                    ctx.parent_value // Use the user-provided trace format
                );

                // trace!("parent_value: {:?}", ctx.parent_value);
                let (ref gtx, ref rid) = ctx
                    .parent_value
                    .downcast_ref::<ErasedRecord>()
                    .ok_or_else(|| internal_error("failed to downcast"))?;

                trace!("Parent is ErasedRecord for path '{}', RID: {}", fd_path, rid);

                match field_kind {
                    // A) Field is Object or Record link (not 'id'): Pass ErasedRecord context down
                    Some(Kind::Object) | Some(Kind::Record(_)) if fd_path != "id" => {
                        trace!("Field at path '{}' is Object/Record, passing down ErasedRecord", fd_path);
                        // let gtx_clone = gtx.clone();
                        // let rid_clone = rid.clone();
                        // let nested_context: ErasedRecord = (gtx_clone, rid_clone);
                        // let field_value = field_val_erase_owned(nested_context);
                        // let field_value = ;
                        // Optional: Add .with_type() hints for record link unions/interfaces here if needed
                        Ok(Some(field_val_erase_owned((gtx.clone(), rid.clone()))))
                    }


                    // C) Field is an Array
                    Some(Kind::Array(inner_kind_box, _)) => {
                        trace!("Field at path '{}' is Array. Inner kind: {:?}", fd_path, inner_kind_box);
                        let db_value_array = gtx.get_record_field(rid.clone(), &fd_path).await?; // Ensure get_record_field is async

                        match db_value_array {
                            SqlValue::Array(surreal_array) => {
                                let inner_kind_ref: &Kind = inner_kind_box.as_ref(); // Get &Kind from
                                // &Box<Kind>
                                let mut gql_item_values = Vec::new();

                                for item_sql_value in surreal_array.0 { // Assuming surreal_array.0 is Vec<SqlValue>
                                    let concrete_item_kind = inner_kind_ref.non_optional();
                                    let item_is_nullable = inner_kind_ref.can_be_none();

                                    if matches!(&item_sql_value, SqlValue::Null | SqlValue::None) {
                                        if item_is_nullable {
                                            gql_item_values.push(FieldValue::value(GqlValue::Null));
                                            continue;
                                        } else {
                                            return Err(internal_error(format!(
                                                "Unexpected null item for non-nullable array element at path '{}', inner kind: {:?}",
                                                fd_path, inner_kind_ref
                                            )).into());
                                        }
                                    }

                                    match concrete_item_kind {
                                        Kind::Record(_) => {
                                            match item_sql_value {
                                                SqlValue::Thing(thing_val) => {
                                                    // Assuming ErasedRecord is (GQLTx, Thing)
                                                    let nested_context: ErasedRecord = (gtx.clone(), thing_val);
                                                    gql_item_values.push(field_val_erase_owned(nested_context));
                                                }
                                                _ => return Err(internal_error(format!(
                                                    "Expected Thing for Record array element at path '{}', got {:?}",
                                                    fd_path, item_sql_value
                                                )).into()),
                                            }
                                        }
                                        // Dynamic Enum: Kind::Either containing only Kind::Literal(Literal::String(_))
                                        Kind::Either(ref ks) if ks.iter().all(|k| matches!(k.non_optional(), Kind::Literal(Literal::String(_)))) => {
                                            match item_sql_value {
                                                SqlValue::Strand(db_string) => { // Ensure Strand is the correct SqlValue variant
                                                    let gql_enum_member = db_string.as_str().to_screaming_snake_case();
                                                    trace!("Dynamic Enum array element: DB '{}' -> GQL '{}' for path {}", db_string.as_str(), gql_enum_member, fd_path);
                                                    gql_item_values.push(FieldValue::value(GqlValue::Enum(Name::new(gql_enum_member))));
                                                }
                                                // // Handle other string-like types if necessary, e.g., SqlValue::String
                                                // SqlValue::String(db_string) => {
                                                //     let gql_enum_member = db_string.as_str().to_screaming_snake_case();
                                                //     gql_item_values.push(FieldValue::value(GqlValue::Enum(Name::new(gql_enum_member))));
                                                // }
                                                _ => return Err(internal_error(format!("Expected String/Strand from DB for dynamic enum in array element at path '{}', got {:?}", fd_path, item_sql_value)).into()),
                                            }
                                        }
                                        // Other scalar types
                                        _ => {
                                            let gql_val = sql_value_to_gql_value(item_sql_value)
                                                .map_err(|e| GqlError::ResolverError(format!("SQL\
                                                 to GQL translation failed for path '{}': {}", fd_path, e)))?;
                                            gql_item_values.push(FieldValue::value(gql_val));
                                        }
                                    }
                                }
                                Ok(Some(FieldValue::list(gql_item_values)))
                            }
                            SqlValue::None | SqlValue::Null => {
                                // The entire array field is null.
                                // This is valid if the GraphQL field for the array is nullable.
                                Ok(None) // async-graphql handles mapping this to `null`
                            }
                            other => {
                                Err(internal_error(format!(
                                    "Expected Array from DB for array field path '{}', got {:?}",
                                    fd_path, other
                                )).into())
                            }
                        }
                    }
                    // B) Field is scalar/terminal/'id': Fetch value using (modified) get_record_field
                    _ => {
                        trace!("Field at path '{}' is scalar/id/terminal, fetching value via get_record_field", fd_path);

                        // Call the modified get_record_field which accepts a path string
                        let sql_value: SqlValue = gtx
                            .get_record_field(rid.clone(), &fd_path)
                            .await?;

                        trace!("Fetched value for path '{}': {:?}", fd_path, sql_value);

                        // --- Convert the fetched value ---
                        match sql_value {
                            SqlValue::None | SqlValue::Null => Ok(None),

                            // This case primarily handles 'id' now or potentially Things returned
                            // unexpectedly for scalar paths.
                            SqlValue::Thing(thing_val) => {
                                trace!("Value for path '{}' is Thing: {}", fd_path, thing_val);
                                let gql_val = sql_value_to_gql_value(SqlValue::Thing(thing_val))?;
                                Ok(Some(FieldValue::value(gql_val)))
                            }

                            // Handle scalars and Enums
                            v => {
                                trace!("Converting value {:?} for path '{}'", v, fd_path);
                                let is_dynamic_enum = match &field_kind { // Use the kind passed to factory
                                    Some(Kind::Option(inner)) => matches!(**inner, Kind::Either(ref ks) if ks.iter().all(|k| matches!(k, Kind::Literal(Literal::String(_))))),
                                    Some(Kind::Either(ref ks)) => ks.iter().all(|k| matches!(k, Kind::Literal(Literal::String(_)))),
                                    _ => false,
                                };

                                let gql_val = if is_dynamic_enum {
                                    match v {
                                        SqlValue::Strand(db_string) => {
                                            let gql_enum_member = db_string.as_str().to_screaming_snake_case();
                                            trace!("Dynamic Enum conversion: DB '{}' -> GQL '{}' for path {}", db_string.as_str(), gql_enum_member, fd_path);
                                            GqlValue::Enum(Name::new(gql_enum_member))
                                        }
                                        _ => return Err(internal_error(format!("Expected String/Strand from DB for dynamic enum at path '{}', got {:?}", fd_path, v)).into())
                                    }
                                } else {
                                    sql_value_to_gql_value(v)
                                        .map_err(|e| GqlError::ResolverError(format!("SQL to GQL translation failed for path '{}': {}", fd_path, e)))?
                                };

                                trace!("Conversion successful for path '{}': {:?}", fd_path, gql_val);
                                Ok(Some(FieldValue::value(gql_val)))
                            }
                        }
                    }
                }
            }
        })
    }
}

// let val = gtx.get_record_field(rid.clone(), fd_name.as_str()).await?;
//
// let out = match val {
//     SqlValue::Thing(rid) if fd_name != "id" => {
//         let mut tmp = field_val_erase_owned((gtx.clone(), rid.clone()));
//         match field_kind {
//             Some(Kind::Record(ts)) if ts.len() != 1 => {
//                 tmp = tmp.with_type(rid.tb.clone())
//             }
//             _ => {}
//         }
//         Ok(Some(tmp))
//     }
//     SqlValue::None | SqlValue::Null => Ok(None),
//     //TODO: Dig here to fix: internal: invalid item for enum \"StatusEnum\"
//     v => {
//         match field_kind {
//             Some(Kind::Either(ks)) if ks.len() != 1 => {}
//             _ => {}
//         }
//         let out = sql_value_to_gql_value(v.to_owned())
//             .map_err(|_| "SQL to GQL translation failed")?;
//         Ok(Some(FieldValue::value(out)))
//     }
// };
// out

// // --- Try Case 1: Parent is the top-level ErasedRecord ---
// if let Some((gtx, rid)) = ctx.parent_value.downcast_ref::<ErasedRecord>() {
//
//     // Fetch the field value directly from the database usiag the record ID
//     let val: SqlValue = gtx
//         .get_record_field(rid.clone(), fd_name.as_str())II
//         .await?; // Handle SurrealError appropriately
//
//
//     // Process the fetched value
//     match val {
//         SqlValue::Thing(nested_rid) if fd_name != "id" => {
//             trace!("Field is a Thing (Record Link) for field {} with nested ID \
//             {}", fd_name, nested_rid);
//             // Wrap the linked record's context for further resolution
//             let erased_nested = (gtx.clone(), nested_rid.clone()); // Clone GQLTx if needed
//             let mut field_value = field_val_erase_owned(erased_nested);
//
//             // Add type hint if it's a union/interface based on Kind::Record
//             match field_kind {
//                 Some(Kind::Record(ref ts)) if ts.len() != 1 => {
//                     field_value = field_value.with_type(nested_rid.tb.clone());
//                 }
//                 Some(Kind::Either(ref _ks)) if matches!(field_kind, Some(Kind::Record(_))) => {
//                     // Handle potential unions defined via Kind::Either containing Kind::Record types?
//                     // This logic might need refinement based on how unions are defined.
//                     // For now, assume Kind::Record handles the primary case.
//                     field_value = field_value.with_type(nested_rid.tb.clone());
//                 }
//                 _ => {}
//             }
//             Ok(Some(field_value))
//         }
//         SqlValue::None | SqlValue::Null => Ok(None),
//         v => {
//             // Convert any other SQL value (Scalar, Object, Array) to GraphQL Value
//             // This includes converting nested SqlValue::Object to GqlValue::Object
//             trace!("Converting fetched scalar/object/array to GQL Value {} for \
//             field {}", v, fd_name);
//             let gql_val = sql_value_to_gql_value(v) // sql_value_to_gql_value MUST handle Objects/Arrays
//                 .map_err(|e| GqlError::ResolverError(format!("SQL to GQL translation failed for field '{}': {}", fd_name, e)))?;
//             // GqlError::new(format!("SQL to GQL conversion error for field '{}': {}", fd_name, e))
//             trace!("Conversion successful! Returning GQL Value {} for field {}", gql_val, fd_name);
//             Ok(Some(FieldValue::value(gql_val))) // Return the converted GQL Value
//         }
//     }
// }
// // --- Try Case 2: Parent is already a resolved GQL Value (for nested fields) ---
// else if let Some(parent_gql_value) = ctx.parent_value.downcast_ref::<GqlValue>() {
//     trace!("Parent is GqlValue (nested) {:?} for field {}", parent_gql_value, fd_name);
//     match parent_gql_value {
//         GqlValue::Object(parent_map) => {
//             // Find the field within the parent GQL object's map
//             // Use the simple field name (e.g., "height") which was passed to this resolver instance
//             let gql_field_name = Name::new(&fd_name); // Use async_graphql::Name for lookup
//
//             if let Some(nested_gql_value) = parent_map.get(&gql_field_name) {
//                 trace!("Found field {} in parent GQL object for field {:?}",
//                     nested_gql_value,
//                     fd_name);
//                 // The value is already a GQL value, just clone and return it
//                 Ok(Some(FieldValue::value(nested_gql_value.clone())))
//             } else {
//                 // Field not found in the parent GQL object map
//                 trace!("Field {} not found in parent GQL \
//                 object", fd_name);
//                 // Return null if the field doesn't exist in the parent map
//                 // (GraphQL handles nullability based on schema type)
//                 Ok(None)
//             }
//         }
//         // Handle if parent is List? Might not occur if lists always resolve fully.
//         // GqlValue::List(_) => { ... }
//         _ => {
//             // Parent was a GqlValue, but not an Object. This indicates a schema mismatch or unexpected state.
//             Err(internal_error(format!(
//                 "Parent value for nested field '{}' was an unexpected GqlValue type: {:?}. Expected Object.",
//                 fd_name, parent_gql_value
//             )).into()) // Ensure error implements Into<async_graphql::Error>
//         }
//     }
// }
// // --- Error Case: Unknown parent type ---
// else {
//     Err(internal_error(format!(
//         "Failed to downcast parent value for field '{}'. Unexpected parent type ID: {:?}",
//         fd_name,
//         ctx.parent_value
//     )).into()) // Ensure error implements Into<async_graphql::Error>
// }


// fn make_nested_field_resolver(
//     base_db_name: String, // e.g., "size"
//     sub_db_name: String,  // e.g., "width"
//     kind: Kind,           // SQL Kind of the sub-field
// ) -> impl for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync + 'static {
//     let full_db_path = format!("{}.{}", base_db_name, sub_db_name); // e.g. "size.width"
//     move |ctx: ResolverContext| {
//         let path = full_db_path.clone();
//         let field_kind = kind.clone(); // Kind of the sub-field itself
//         FieldFuture::new(async move {
//             // Parent value should be the ErasedRecord of the CONTAINER object (e.g., Image)
//             let (ref gtx, ref parent_rid) = ctx
//                 .parent_value
//                 .downcast_ref::<ErasedRecord>()
//                 .ok_or_else(|| internal_error(format!("failed to downcast parent for nested field {}", path)))?;
//
//             // Fetch using the full path
//             let val = gtx.get_record_field(parent_rid.clone(), &path).await?;
//
//             // Use the SAME conversion logic as make_table_field_resolver's final match arm
//             // (including the Enum fix)
//             let out = match val {
//                 SqlValue::None | SqlValue::Null => Ok(None),
//                 // Nested fields shouldn't typically be Things unless it's object<record<...>>
//                 SqlValue::Thing(_) => Err(internal_error(format!("Unexpected Thing found for nested field {}", path))),
//                 v => { // Handle scalars and potential Enums
//                     // Use the kind of the *sub-field* here
//                     let is_dynamic_enum = match &field_kind {
//                         Some(Kind::Option(inner)) => matches!(**inner, Kind::Either(ref ks) if ks.iter().all(|k| matches!(k, Kind::Literal(Literal::String(_))))),
//                         Some(Kind::Either(ref ks)) => ks.iter().all(|k| matches!(k, Kind::Literal(Literal::String(_)))),
//                         _ => false,
//                     };
//
//                     if is_dynamic_enum {
//                         // match v.to_string_lossy() { // Adjust based on SqlValue string access
//                         //     Some(db_string) => {
//                         //         let gql_enum_value_str = db_string.to_screaming_snake_case();
//                         //         let gql_enum_value = GqlValue::Enum(Name::new(gql_enum_value_str));
//                         //         Ok(Some(FieldValue::value(gql_enum_value)))
//                         //     }
//                         //     None => Ok(None), // Or error
//                         // }
//                         match v {
//                             SqlValue::Strand(s) => {
//                                 let db_string = s.as_str();
//                                 let gql_enum_value_str = db_string.to_screaming_snake_case();
//
//                                 // Use Name::new (panics on invalid GraphQL name chars, unlikely here)
//                                 // Or implement a validation check if needed before calling new()
//                                 let gql_enum_value = GqlValue::Enum(Name::new(gql_enum_value_str)); // FIX: Use Name::new
//
//                                 Ok(Some(FieldValue::value(gql_enum_value)))
//                             }
//                             // Add other SqlValue variants if they can represent your enum strings
//                             _ => {
//                                 // FIX: Use the correct variable name for the error message
//                                 error!("Expected a Strand from DB for dynamic enum field '{}', but got different value: {:?}", db_name, v); // Use db_name (from resolver args) or path (from nested resolver args)
//                                 Ok(None)
//                             }
//                         }
//                     } else {
//                         // Generic conversion
//                         let gql_value = sql_value_to_gql_value(v) // Pass owned v if needed
//                             .map_err(|e| format!("SQL to GQL translation failed for nested field '{}': {}", path, e))?;
//                         Ok(Some(FieldValue::value(gql_value)))
//                     }
//                 }
//             };
//             out
//         })
//     }
// }
//
// fn make_table_field_resolver(
//     db_name: String, // DB name (e.g., "created_at", "size")
//     kind: Option<Kind>,
// ) -> impl for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync + 'static {
//     move |ctx: ResolverContext| {
//         let fd_name = db_name.clone(); // Use db_name passed in
//         let field_kind = kind.clone();
//         FieldFuture::new({
//             async move {
//                 let (ref gtx, ref rid) = ctx
//                     .parent_value
//                     .downcast_ref::<ErasedRecord>()
//                     .ok_or_else(|| internal_error(format!("failed to downcast parent for field {}", fd_name)))?;
//
//                 // If this field represents the BASE of a nested object (e.g. "size"),
//                 // we don't fetch its value directly. Instead, we just pass the parent's
//                 // ErasedRecord down, so the nested field resolvers can use it.
//                 if matches!(field_kind, Some(Kind::Object)) {
//                     // Check if it actually has nested structure defined via dot notation
//                     // (This check might need refinement based on how Kind::Object is populated)
//                     // For now, assume if kind is Object, we pass parent context down.
//                     let parent_erased_record = (gtx.clone(), rid.clone());
//                     // Wrap it so async-graphql knows how to handle it as the parent for sub-fields
//                     return Ok(Some(field_val_erase_owned(parent_erased_record)));
//                 }
//
//                 // Otherwise, fetch the field value as before
//                 let val = gtx.get_record_field(rid.clone(), fd_name.as_str()).await?;
//
//                 let out = match val {
//                     SqlValue::Thing(related_rid) if fd_name != "id" => { // Handle relations
//                         let mut tmp = field_val_erase_owned((gtx.clone(), related_rid.clone()));
//                         match &field_kind {
//                             Some(Kind::Record(ts)) if ts.len() != 1 => {
//                                 tmp = tmp.with_type(related_rid.tb.clone())
//                             }
//                             _ => {}
//                         }
//                         Ok(Some(tmp))
//                     }
//                     SqlValue::None | SqlValue::Null => Ok(None),
//                     v => { // Handle scalars and Enums
//                         let is_dynamic_enum = match &field_kind {
//                             Some(Kind::Option(inner)) => matches!(**inner, Kind::Either(ref ks) if ks.iter().all(|k| matches!(k, Kind::Literal(Literal::String(_))))),
//                             Some(Kind::Either(ref ks)) => ks.iter().all(|k| matches!(k, Kind::Literal(Literal::String(_)))),
//                             _ => false,
//                         };
//
//                         if is_dynamic_enum {
//                             match v.to_string_lossy() { // Adjust string access as needed
//                                 Some(db_string) => {
//                                     let gql_enum_value_str = db_string.to_screaming_snake_case();
//                                     let gql_enum_value = GqlValue::Enum(Name::new(gql_enum_value_str));
//                                     Ok(Some(FieldValue::value(gql_enum_value)))
//                                 }
//                                 None => Ok(None), // Or error
//                             }
//                         } else {
//                             // Generic conversion
//                             let gql_value = sql_value_to_gql_value(v) // Pass owned v if needed
//                                 .map_err(|e| format!("SQL to GQL translation failed for field '{}': {}", fd_name, e))?;
//                             Ok(Some(FieldValue::value(gql_value)))
//                         }
//                     }
//                 };
//                 out
//             }
//         })
//     }
// }

macro_rules! filter_impl {
	($filter:ident, $ty:ident, $name:expr) => {
		$filter = $filter.field(InputValue::new(format!("{}", $name), $ty.clone()));
	};
}

//FIXME: implement
fn page_info_resolver(
    db_name: String, // DB name (e.g., "created_at", "size")
    kind: Option<Kind>,
) -> impl for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync + 'static {
    move |_ctx: ResolverContext| {
        FieldFuture::new(async move {
            Ok(Some(FieldValue::value("".to_string()))) // Return `None` as a placeholder
        })
    }
}

fn filter_id() -> InputObject {
    let mut filter = InputObject::new("IDFilterInput");
    let ty = TypeRef::named(TypeRef::ID);
    filter_impl!(filter, ty, "eq");
    filter_impl!(filter, ty, "ne");
    filter
}
fn filter_from_type(
    kind: Kind,
    filter_name: String,
    types: &mut Vec<Type>,
) -> Result<InputObject, GqlError> {
    let ty = match &kind {
        Kind::Record(ts) => match ts.len() {
            1 => TypeRef::named(TypeRef::ID),
            _ => TypeRef::named(filter_name_from_table(
                ts.first().expect("ts should have exactly one element").as_str(),
            )),
        },
        //TODO: remove none
        // k => unwrap_type(kind_to_type(k.clone(), types, None)?),
        k => TypeRef::named("UNIMPLEMENTED"),
    };

    let mut filter = InputObject::new(filter_name);
    filter_impl!(filter, ty, "eq");
    filter_impl!(filter, ty, "ne");

    match kind {
        Kind::Any => {}
        Kind::Null => {}
        Kind::Bool => {}
        Kind::Bytes => {}
        Kind::Datetime => {}
        Kind::Decimal => {}
        Kind::Duration => {}
        Kind::Float => {}
        Kind::Int => {}
        Kind::Number => {}
        Kind::Object => {}
        Kind::Point => {}
        Kind::String => {}
        Kind::Uuid => {}
        Kind::Regex => {}
        Kind::Record(_) => {}
        Kind::Geometry(_) => {}
        Kind::Option(_) => {}
        Kind::Either(_) => {}
        Kind::Set(_, _) => {}
        Kind::Array(_, _) => {}
        Kind::Function(_, _) => {}
        Kind::Range => {}
        Kind::Literal(_) => {}
        Kind::References(_, _) => {}
        Kind::File(_) => {}
    };
    Ok(filter)
}

// fn cond_from_filter(
//     filter: &IndexMap<Name, GqlValue>,
//     fds: &[DefineFieldStatement],
// ) -> Result<Cond, GqlError> {
//     // val_from_filter(filter, fds).map(IntoExt::intox)
//     // Start recursion with an empty path prefix
//     val_from_filter(filter, fds, &[]).map(IntoExt::intox)
// }

// fn val_from_filter(
//     filter: &IndexMap<Name, GqlValue>,
//     fds: &[DefineFieldStatement],
//     current_path: &[String],
// ) -> Result<SqlValue, GqlError> {
//     if filter.len() != 1 {
//         let path_str = current_path.join(".");
//         return Err(resolver_error(format!("Filter object at path '{}' must have exactly one key (field, and, or, not)", path_str)));
//     }
//
//     let (k, v) = filter.iter().next().unwrap();
//     let key_str = k.as_str();
//
//     let cond = match key_str.to_lowercase().as_str() { // Keep matching lowercase for operators
//         "or" => aggregate(v, AggregateOp::Or, fds, current_path), // Pass path down
//         "and" => aggregate(v, AggregateOp::And, fds, current_path), // Pass path down
//         "not" => negate(v, fds, current_path), // Pass path down
//         _ => { // Assume it's a field name (camelCase from schema)
//             // Construct the new path segment
//             let mut next_path = current_path.to_vec();
//             next_path.push(key_str.to_string()); // Add the camelCase field name
//
//             // Find the DB field definition matching the potential full path
//             // This might require looking up the base field and checking if it's an object,
//             // then checking the sub-field within the nested structure.
//             // For simplicity here, we'll assume we can find the field kind based on the path.
//             let field_kind = find_field_kind_by_path(&next_path, fds)?; // Implement this helper
//
//             match field_kind {
//                 // If the path points to a nested object, recurse
//                 Kind::Object => {
//                     let inner_filter = v.as_object().ok_or_else(|| resolver_error(format!("Value for object filter '{}' must be an object", next_path.join("."))))?;
//                     val_from_filter(inner_filter, fds, &next_path) // Recurse with extended path
//                 }
//                 // If it's a scalar/record/enum etc., call binop
//                 _ => Ok({
//                     binop(&next_path, v, field_kind)? // Pass full path and kind
//                 })
//             }
//         }
//     };
//
//     cond
//     // if filter.len() != 1 {
//     // 	return Err(resolver_error("Table Filter must have one item"));
//     // }
//     //
//     // let (k, v) = filter.iter().next().unwrap();
//     //
//     // let cond = match k.as_str().to_lowercase().as_str() {
//     // 	"or" => aggregate(v, AggregateOp::Or, fds),
//     // 	"and" => aggregate(v, AggregateOp::And, fds),
//     // 	"not" => negate(v, fds),
//     // 	_ => binop(k.as_str(), v, fds),
//     // };
//     //
//     // cond
// }

fn parse_op(name: impl AsRef<str>) -> Result<sql::Operator, GqlError> {
    match name.as_ref() {
        "eq" => Ok(sql::Operator::Equal),
        "ne" => Ok(sql::Operator::NotEqual),
        op => Err(resolver_error(format!("Unsupported op: {op}"))),
    }
}

fn find_field_kind_by_path(path: &[String], fds: &Arc<Vec<DefineFieldStatement>>) -> Result<Kind, GqlError> {
    // Convert GQL camelCase path back to DB snake_case/dot.notation path
    // This assumes a simple reversible mapping, might need adjustment
    let db_path_str = path.iter()
        .map(|p| p.to_snake_case()) // Convert each segment
        .collect::<Vec<_>>()
        .join("."); // Join with dots

    fds.iter()
        .find(|fd| fd.name.to_string() == db_path_str)
        .and_then(|fd| fd.kind.clone())
        .ok_or_else(|| resolver_error(format!("Field definition not found for path '{}' (DB path '{}')", path.join("."), db_path_str)))
}

// fn negate(filter: &GqlValue, fds: &Arc<Vec<DefineFieldStatement>>, current_path: &[String]) -> Result<SqlValue, GqlError> {
//     let obj = filter.as_object().ok_or(resolver_error("Value of NOT must be object"))?;
//
//     let inner_cond = val_from_filter(obj, fds, current_path)?;
//     Ok(Expression::Unary { o: sql::Operator::Not, v: inner_cond }.into())
// }

enum AggregateOp {
    And,
    Or,
}

// fn aggregate(
//     filter: &GqlValue,
//     op: AggregateOp,
//     fds: &Arc<Vec<DefineFieldStatement>>,
//     current_path: &[String],
// ) -> Result<SqlValue, GqlError> {
//     let op_str = match op {
//         AggregateOp::And => "AND",
//         AggregateOp::Or => "OR",
//     };
//     let op = match op {
//         AggregateOp::And => sql::Operator::And,
//         AggregateOp::Or => sql::Operator::Or,
//     };
//     let list =
//         filter.as_list().ok_or(resolver_error(format!("Value of {op_str} should be a list")))?;
//     let filter_arr = list
//         .iter()
//         .map(|v| v.as_object().map(|o| val_from_filter(o, fds, current_path)))
//         .collect::<Option<Result<Vec<SqlValue>, GqlError>>>()
//         .ok_or(resolver_error(format!("List of {op_str} should contain objects")))??;
//
//     let mut iter = filter_arr.into_iter();
//
//     let mut cond = iter
//         .next()
//         .ok_or(resolver_error(format!("List of {op_str} should contain at least one object")))?;
//
//     for clause in iter {
//         cond = Expression::Binary {
//             l: clause,
//             o: op.clone(),
//             r: cond,
//         }
//             .into();
//     }
//
//     Ok(cond)
// }

fn binop(
    gql_path: &[String], // e.g., ["size", "width"]
    val: &GqlValue,     // e.g., { eq: 100 }
    field_kind: Kind, // The Kind of the specific field at the end of the path
) -> Result<SqlValue, GqlError> {
    let obj = val.as_object().ok_or_else(|| resolver_error(format!("Filter value for '{}' must be an object", gql_path.join("."))))?;

    if obj.len() != 1 {
        return Err(resolver_error(format!("Filter operation object for '{}' must have exactly one key (e.g., eq, gt)", gql_path.join("."))));
    }

    // Convert GQL path (camelCase) back to DB path (snake_case.dot) for SQL Idiom
    // ASSUMPTION: Simple reversible mapping. May need adjustment.
    let db_path_str = gql_path.iter().map(|p| p.to_snake_case()).collect::<Vec<_>>().join(".");
    let lhs = sql::Value::Idiom(db_path_str.intox()); // Use the full DB path

    let (k, v) = obj.iter().next().unwrap(); // k is the operator name (e.g., "eq")
    let op = parse_op(k)?; // Parse "eq", "ne", etc. (Needs expansion)

    // Convert the GQL value 'v' (e.g., Number(100)) to SQL using the specific field's Kind
    let rhs = gql_to_sql_kind(v, field_kind)?;

    Ok(sql::Expression::Binary { l: lhs, o: op, r: rhs }.into())
}

fn parse_order_input(order: Option<&GqlValue>) -> Result<Option<Vec<sql::Order>>, GqlError> {
    let Some(GqlValue::Object(o)) = order else { return Ok(None) };

    let mut orders = vec![];
    let mut current = o;

    loop {
        let Some(GqlValue::Enum(field_name_enum)) = current.get("field") else {
            return Err(resolver_error("Order input must contain 'field' enum"));
        };
        let Some(GqlValue::Enum(direction_enum)) = current.get("direction") else {
            return Err(resolver_error("Order input must contain 'direction' enum (ASC/DESC)"));
        };

        let field_name_screaming = field_name_enum.as_str(); // e.g., "CREATED_AT", "SIZE_WIDTH"
        // Convert SCREAMING_SNAKE_CASE back to DB snake_case.dot notation
        let db_field_name = field_name_screaming.to_lowercase(); // Simple conversion, might need underscores replaced with dots

        let direction_is_asc = direction_enum.as_str() == "ASC";

        let mut order_clause = sql::Order::default();
        order_clause.value = db_field_name.into(); // Use DB name/path
        order_clause.direction = direction_is_asc;
        orders.push(order_clause);

        // Check for chained 'then'
        if let Some(GqlValue::Object(next)) = current.get("then") {
            current = next;
        } else {
            break;
        }
    }
    Ok(Some(orders))
}


//TODO: resolve with get_record_field funktioniert fuer ein level nested.
// hier bei 'size.location.`info`' findet er obvious nicht: None -> Error


// TODO: auch bei .url oder users.0.image.size.height
// weil hier path: size.height und der nicht gefunden wird fuer query
// SELECT * FROM user:id


// 2025-04-22T19:32:25.885157Z TRACE request: surrealdb_core::gql::tables: /Volumes/Development/Dev/RustroverProjects/surrealdb/crates/core/src/gql/tables.rs:659: Creating/Running resolver for DB path 'size.location.`info`' (Kind: Some(Float)) with parent: (surrealdb_core::gql::utils::GQLTx, surrealdb_core::sql::thing::Thing)     otel.kind="server" http.request.method="POST" url.path="/graphql" network.protocol.name="http" network.protocol.version="1.1" http.request.body.size="244" user_agent.original="PostmanClient/11.36.1 (AppId=90094569-b9aa-467a-902b-a9c21e5e66af)" otel.name="POST /graphql" http.route="/graphql" http.request.id="ff04a2ef-5af1-48dd-a50d-4d1ad73bd444" client.address="127.0.0.1"
// 2025-04-22T19:32:25.885164Z TRACE request: surrealdb_core::gql::tables: /Volumes/Development/Dev/RustroverProjects/surrealdb/crates/core/src/gql/tables.rs:672: Parent is ErasedRecord for path 'size.location.`info`', RID: image:ppli74w1kj8biujquu43     otel.kind="server" http.request.method="POST" url.path="/graphql" network.protocol.name="http" network.protocol.version="1.1" http.request.body.size="244" user_agent.original="PostmanClient/11.36.1 (AppId=90094569-b9aa-467a-902b-a9c21e5e66af)" otel.name="POST /graphql" http.route="/graphql" http.request.id="ff04a2ef-5af1-48dd-a50d-4d1ad73bd444" client.address="127.0.0.1"
// 2025-04-22T19:32:25.885172Z TRACE request: surrealdb_core::gql::tables: /Volumes/Development/Dev/RustroverProjects/surrealdb/crates/core/src/gql/tables.rs:688: Field at path 'size.location.`info`' is scalar/id/terminal, fetching value via get_record_field     otel.kind="server" http.request.method="POST" url.path="/graphql" network.protocol.name="http" network.protocol.version="1.1" http.request.body.size="244" user_agent.original="PostmanClient/11.36.1 (AppId=90094569-b9aa-467a-902b-a9c21e5e66af)" otel.name="POST /graphql" http.route="/graphql" http.request.id="ff04a2ef-5af1-48dd-a50d-4d1ad73bd444" client.address="127.0.0.1"
// 2025-04-22T19:32:25.885188Z TRACE request: surrealdb::core::dbs: crates/core/src/dbs/iterator.rs:354: Iterating statement statement=SELECT * FROM image:ppli74w1kj8biujquu43 otel.kind="server" http.request.method="POST" url.path="/graphql" network.protocol.name="http" network.protocol.version="1.1" http.request.body.size="244" user_agent.original="PostmanClient/11.36.1 (AppId=90094569-b9aa-467a-902b-a9c21e5e66af)" otel.name="POST /graphql" http.route="/graphql" http.request.id="ff04a2ef-5af1-48dd-a50d-4d1ad73bd444" client.address="127.0.0.1"
// 2025-04-22T19:32:25.885258Z TRACE request: surrealdb_core::gql::tables: /Volumes/Development/Dev/RustroverProjects/surrealdb/crates/core/src/gql/tables.rs:695: Fetched value for path 'size.location.`info`': None     otel.kind="server" http.request.method="POST" url.path="/graphql" network.protocol.name="http" network.protocol.version="1.1" http.request.body.size="244" user_agent.original="PostmanClient/11.36.1 (AppId=90094569-b9aa-467a-902b-a9c21e5e66af)" otel.name="POST /graphql" http.route="/graphql" http.request.id="ff04a2ef-5af1-48dd-a50d-4d1ad73bd444" client.address="127.0.0.1"
