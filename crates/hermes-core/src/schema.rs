//! JSON Schema generation for tool definitions
//!
//! Automatically generates OpenAI-compatible JSON Schema definitions from Rust structs
//! using the `schemars` crate.

use schemars::schema::{InstanceType, Schema, SchemaObject, StringValidation};
use schemars::{schema_for, JsonSchema};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Represents a tool's JSON Schema definition
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolSchema {
    /// Tool name (e.g., "get_weather")
    pub name: String,
    /// Human-readable description of what the tool does
    pub description: String,
    /// JSON Schema for the tool's parameters
    pub parameters: Value,
}

impl ToolSchema {
    /// Create a new ToolSchema
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// Generate schema from a type that implements JsonSchema
    pub fn from_type<T: JsonSchema>(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let parameters = schema_for!(T);
        let parameters_value = serde_json::to_value(&parameters).unwrap_or_else(|_| {
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            })
        });

        Self {
            name: name.into(),
            description: description.into(),
            parameters: parameters_value,
        }
    }

    /// Validate arguments against the schema
    pub fn validate_args(&self, args: &Value) -> Result<()> {
        // Basic validation - check if args is an object
        if !args.is_object() {
            return Err(Error::InvalidToolArgs {
                name: self.name.clone(),
                details: "Arguments must be a JSON object".to_string(),
            });
        }

        // For now, we do basic structural validation
        // A full JSON Schema validator would be more robust
        let params_obj = self.parameters.get("properties");
        if let Some(props) = params_obj.and_then(|p| p.as_object()) {
            for (key, _schema) in props {
                // Check required fields
                if let Some(required) = self.parameters.get("required") {
                    if let Some(reqs) = required.as_array() {
                        let required_keys: Vec<&str> =
                            reqs.iter().filter_map(|v| v.as_str()).collect();
                        if required_keys.contains(&key.as_str()) && args.get(key).is_none() {
                            return Err(Error::InvalidToolArgs {
                                name: self.name.clone(),
                                details: format!("Missing required field: {}", key),
                            });
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Generate an OpenAI-compatible tools array from a list of ToolSchemas
pub fn to_openai_tools(schemas: &[ToolSchema]) -> Value {
    let tools: Vec<Value> = schemas
        .iter()
        .map(|schema| {
            json!({
                "type": "function",
                "function": {
                    "name": schema.name,
            "description": schema.description,
                    "parameters": schema.parameters
                }
            })
        })
        .collect();

    json!({ "tools": tools })
}

/// Schema generator for creating custom schemas programmatically
pub struct SchemaGenerator;

impl SchemaGenerator {
    /// Create a schema for a string parameter
    pub fn string_param(name: &str, description: &str) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                max_length: None,
                min_length: None,
                pattern: None,
            })),
            metadata: Some(Box::new(schemars::schema::Metadata {
                id: None,
                title: Some(name.to_string()),
                description: Some(description.to_string()),
                default: None,
                deprecated: false,
                read_only: false,
                write_only: false,
                examples: vec![],
            })),
            ..Default::default()
        })
    }

    /// Create a schema for an integer parameter
    pub fn integer_param(name: &str, description: &str) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Integer.into()),
            metadata: Some(Box::new(schemars::schema::Metadata {
                id: None,
                title: Some(name.to_string()),
                description: Some(description.to_string()),
                default: None,
                deprecated: false,
                read_only: false,
                write_only: false,
                examples: vec![],
            })),
            ..Default::default()
        })
    }

    /// Create a schema for a boolean parameter
    pub fn boolean_param(name: &str, description: &str) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Boolean.into()),
            metadata: Some(Box::new(schemars::schema::Metadata {
                id: None,
                title: Some(name.to_string()),
                description: Some(description.to_string()),
                default: None,
                deprecated: false,
                read_only: false,
                write_only: false,
                examples: vec![],
            })),
            ..Default::default()
        })
    }

    /// Create a schema for an array parameter
    pub fn array_param(name: &str, description: &str, items: Schema) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Array.into()),
            array: Some(Box::new(schemars::schema::ArrayValidation {
                items: Some(items.into()),
                ..Default::default()
            })),
            metadata: Some(Box::new(schemars::schema::Metadata {
                id: None,
                title: Some(name.to_string()),
                description: Some(description.to_string()),
                default: None,
                deprecated: false,
                read_only: false,
                write_only: false,
                examples: vec![],
            })),
            ..Default::default()
        })
    }

    /// Create a schema for an object parameter
    pub fn object_param(name: &str, description: &str, properties: Vec<(&str, Schema)>) -> Schema {
        let mut obj = schemars::schema::ObjectValidation::default();
        for (prop_name, prop_schema) in properties {
            obj.properties.insert(prop_name.to_string(), prop_schema);
        }

        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Object.into()),
            object: Some(Box::new(obj)),
            metadata: Some(Box::new(schemars::schema::Metadata {
                id: None,
                title: Some(name.to_string()),
                description: Some(description.to_string()),
                default: None,
                deprecated: false,
                read_only: false,
                write_only: false,
                examples: vec![],
            })),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct TestParams {
        query: String,
        limit: Option<i32>,
    }

    #[test]
    fn test_schema_generation() {
        let schema = ToolSchema::from_type::<TestParams>("test_tool", "A test tool");

        assert_eq!(schema.name, "test_tool");
        assert_eq!(schema.description, "A test tool");
        assert!(schema.parameters.is_object());
    }

    #[test]
    fn test_openai_tools_format() {
        let schemas = vec![
            ToolSchema::from_type::<TestParams>("tool1", "First tool"),
            ToolSchema::from_type::<TestParams>("tool2", "Second tool"),
        ];

        let tools = to_openai_tools(&schemas);

        assert!(tools.get("tools").is_some());
        let tools_array = tools.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools_array.len(), 2);
    }
}
