use std::{backtrace, fmt::Debug};

use async_graphql::{InputType, InputValueError};
use thiserror::Error;

use crate::sql::Kind;

#[derive(Error, Debug)]
pub enum GqlError {
    #[error("Database error: {0}")]
    DbError(crate::err::Error),
    #[error("Error generating schema: {0}")]
    SchemaError(String),
    #[error("Error resolving request: {0}")]
    ResolverError(String),
    #[error("No Namespace specified")]
    UnspecifiedNamespace,
    #[error("No Database specified")]
    UnspecifiedDatabase,
    #[error("GraphQL has not been configured for this database")]
    NotConfigured,
    #[error("Internal Error: {0}")]
    InternalError(String),
    #[error("Input Error: {0}")]
    InputError(String),
    #[error("Error converting value: {val} to type: {target}")]
    TypeError {
        target: Kind,
        val: async_graphql::Value,
    },
}

pub fn schema_error(msg: impl Into<String>) -> GqlError {
    GqlError::SchemaError(msg.into())
}

pub fn resolver_error(msg: impl Into<String>) -> GqlError {
    GqlError::ResolverError(msg.into())
}

pub fn input_error(msg: impl Into<String>) -> GqlError {
    let msg = msg.into();
    let bt = backtrace::Backtrace::capture();

    error!("{}\n{bt}", msg);
    GqlError::InputError(msg)
}

pub fn internal_error(msg: impl Into<String>) -> GqlError {
    let msg = msg.into();
    let bt = backtrace::Backtrace::capture();

    error!("{}\n{bt}", msg);
    GqlError::InternalError(msg)
}

pub fn type_error(kind: Kind, val: &async_graphql::Value) -> GqlError {
    GqlError::TypeError {
        target: kind,
        val: val.to_owned(),
    }
}

impl From<crate::err::Error> for GqlError {
    fn from(value: crate::err::Error) -> Self {
        GqlError::DbError(value)
    }
}

impl<T> From<InputValueError<T>> for GqlError
where
    T: InputType + Debug,
{
    fn from(value: InputValueError<T>) -> Self {
        GqlError::ResolverError(format!("{value:?}"))
    }
}
