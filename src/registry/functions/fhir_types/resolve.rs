//! resolve() function - resolves FHIR references to resources

use crate::model::{FhirPathValue, FhirResource, TypeInfo};
use crate::registry::function::{
    EvaluationContext, FhirPathFunction, FunctionError, FunctionResult,
};
use crate::registry::signature::FunctionSignature;
use serde_json::Value;

/// resolve() function - resolves FHIR references to resources
///
/// For each item in the collection, if it is a string that is a uri (or canonical or url),
/// locate the target of the reference, and add it to the resulting collection.
/// If the item does not resolve to a resource, the item is ignored and nothing is added
/// to the output collection. The items in the collection may also represent a Reference,
/// in which case the Reference.reference is resolved.
pub struct ResolveFunction;

impl FhirPathFunction for ResolveFunction {
    fn name(&self) -> &str {
        "resolve"
    }

    fn human_friendly_name(&self) -> &str {
        "Resolve Reference"
    }

    fn signature(&self) -> &FunctionSignature {
        static SIG: std::sync::LazyLock<FunctionSignature> = std::sync::LazyLock::new(|| {
            FunctionSignature::new(
                "resolve",
                vec![], // No parameters - operates on the input collection
                TypeInfo::Collection(Box::new(TypeInfo::Any)),
            )
        });
        &SIG
    }

    fn evaluate(
        &self,
        args: &[FhirPathValue],
        context: &EvaluationContext,
    ) -> FunctionResult<FhirPathValue> {
        // resolve() takes no arguments
        if !args.is_empty() {
            return Err(FunctionError::InvalidArity {
                name: self.name().to_string(),
                min: 0,
                max: Some(0),
                actual: args.len(),
            });
        }

        let mut resolved_resources = Vec::new();

        // Process the input collection
        let items = match &context.input {
            FhirPathValue::Collection(items) => items.iter().collect::<Vec<_>>(),
            FhirPathValue::Empty => return Ok(FhirPathValue::Empty),
            single => vec![single],
        };

        for item in items {
            if let Some(resolved) = self.resolve_item(item, context) {
                resolved_resources.push(resolved);
            }
        }

        Ok(FhirPathValue::collection(resolved_resources))
    }
}

impl ResolveFunction {
    /// Resolve a single item (reference string or Reference resource)
    fn resolve_item(
        &self,
        item: &FhirPathValue,
        context: &EvaluationContext,
    ) -> Option<FhirPathValue> {
        match item {
            // Handle string URIs/references
            FhirPathValue::String(uri) => self.resolve_string_reference(uri, context),

            // Handle Reference resources
            FhirPathValue::Resource(resource) => {
                if self.is_reference(resource) {
                    self.resolve_reference_resource(resource, context)
                } else {
                    None
                }
            }

            _ => None,
        }
    }

    /// Check if a resource is a Reference type
    fn is_reference(&self, resource: &FhirResource) -> bool {
        resource
            .as_json()
            .as_object()
            .map_or(false, |obj| obj.contains_key("reference"))
    }

    /// Resolve a Reference resource by extracting its reference field
    fn resolve_reference_resource(
        &self,
        resource: &FhirResource,
        context: &EvaluationContext,
    ) -> Option<FhirPathValue> {
        let obj = resource.as_json().as_object()?;
        let reference_str = obj.get("reference")?.as_str()?;
        self.resolve_string_reference(reference_str, context)
    }

    /// Resolve a string reference (URI/URL)
    fn resolve_string_reference(
        &self,
        reference: &str,
        context: &EvaluationContext,
    ) -> Option<FhirPathValue> {
        // Trim whitespace and skip anything that doesn’t look like a FHIR reference
        let ref_str = reference.trim();
        if !ref_str.starts_with('#') && !self.is_fhir_reference(ref_str) {
            return None;
        }

        // 1. Fragment to contained resource
        if let Some(contained_id) = ref_str.strip_prefix('#') {
            return self.resolve_contained_resource(contained_id, context);
        }

        // Helpers ----------------------------------------------------------------

        // Parse "Type/ID" from references like "Patient/123" or full URLs.
        fn parse_reference_type_id(reference: &str) -> Option<(String, String)> {
            let parts: Vec<&str> = reference.split('/').collect();
            if parts.len() < 2 {
                return None;
            }
            let (t, raw_id) = (
                parts[parts.len() - 2],
                parts[parts.len() - 1]
                    .split(|c| c == '?' || c == '#')
                    .next()
                    .unwrap_or(""),
            );
            Some((t.to_string(), raw_id.to_string()))
        }

        // Search entries in a Bundle
        fn resolve_in_bundle(reference: &str, bundle: &FhirResource) -> Option<FhirPathValue> {
            let bundle_json = bundle.as_json();
            let entries = bundle_json.get("entry")?.as_array()?;

            // Fast path: fullUrl match
            for entry in entries {
                if let Some(full_url) = entry.get("fullUrl").and_then(|v| v.as_str()) {
                    if full_url == reference {
                        let r = entry.get("resource")?.clone();
                        return Some(FhirPathValue::Resource(FhirResource::from_json(r)));
                    }
                }
            }

            // Parse type/id
            if let Some((ref_type, ref_id)) = parse_reference_type_id(reference) {
                // Match resourceType/id
                for entry in entries {
                    let res = entry.get("resource")?;
                    let obj = res.as_object()?;
                    if obj.get("resourceType")?.as_str()? == ref_type
                        && obj.get("id")?.as_str()? == ref_id
                    {
                        return Some(FhirPathValue::Resource(FhirResource::from_json(
                            res.clone(),
                        )));
                    }
                }
                // Fallback: parse on fullUrl tail
                for entry in entries {
                    if let Some(full_url) = entry.get("fullUrl").and_then(|v| v.as_str()) {
                        if let Some((t, id)) = parse_reference_type_id(full_url) {
                            if t == ref_type && id == ref_id {
                                let r = entry.get("resource")?.clone();
                                return Some(FhirPathValue::Resource(FhirResource::from_json(r)));
                            }
                        }
                    }
                }
            }

            None
        }

        // Search entries in a Parameters resource
        fn resolve_in_parameters(
            reference: &str,
            parameters: &FhirResource,
        ) -> Option<FhirPathValue> {
            let params = parameters.as_json().get("parameter")?.as_array()?;
            if let Some((ref_type, ref_id)) = parse_reference_type_id(reference) {
                // Recursively search `parameter` and nested `part`
                fn search_params(
                    array: &[Value],
                    r_type: &str,
                    r_id: &str,
                ) -> Option<FhirPathValue> {
                    for param in array {
                        if let Some(res) = param.get("resource") {
                            let obj = res.as_object()?;
                            if obj.get("resourceType")?.as_str()? == r_type
                                && obj.get("id")?.as_str()? == r_id
                            {
                                return Some(FhirPathValue::Resource(FhirResource::from_json(
                                    res.clone(),
                                )));
                            }
                        }
                        if let Some(parts) = param.get("part").and_then(|v| v.as_array()) {
                            if let Some(found) = search_params(parts, r_type, r_id) {
                                return Some(found);
                            }
                        }
                    }
                    None
                }
                return search_params(params, &ref_type, &ref_id);
            }
            None
        }

        // Check the root resource itself
        fn resolve_against_resource(
            reference: &str,
            resource: &FhirResource,
        ) -> Option<FhirPathValue> {
            if let Some((ref_type, ref_id)) = parse_reference_type_id(reference) {
                if resource.resource_type()? == ref_type {
                    let obj = resource.as_json().as_object()?;
                    if obj.get("id")?.as_str()? == ref_id {
                        return Some(FhirPathValue::Resource(resource.clone()));
                    }
                }
            }
            None
        }

        // Main resolution against context.root -----------------------------------
        match &context.root {
            FhirPathValue::Resource(root_res) => {
                if let Some(rt) = root_res.resource_type() {
                    if rt == "Bundle" {
                        if let Some(found) = resolve_in_bundle(ref_str, root_res) {
                            return Some(found);
                        }
                    } else if rt == "Parameters" {
                        if let Some(found) = resolve_in_parameters(ref_str, root_res) {
                            return Some(found);
                        }
                    }
                }
                if let Some(found) = resolve_against_resource(ref_str, root_res) {
                    return Some(found);
                }
            }
            FhirPathValue::Collection(coll) => {
                for val in coll.iter() {
                    if let FhirPathValue::Resource(res) = val {
                        if let Some(rt) = res.resource_type() {
                            if rt == "Bundle" {
                                if let Some(found) = resolve_in_bundle(ref_str, res) {
                                    return Some(found);
                                }
                            } else if rt == "Parameters" {
                                if let Some(found) = resolve_in_parameters(ref_str, res) {
                                    return Some(found);
                                }
                            }
                        }
                        if let Some(found) = resolve_against_resource(ref_str, res) {
                            return Some(found);
                        }
                    }
                }
            }
            _ => {}
        }

        None
    }

    /// Resolve a contained resource by ID
    fn resolve_contained_resource(
        &self,
        id: &str,
        context: &EvaluationContext,
    ) -> Option<FhirPathValue> {
        if let FhirPathValue::Resource(root_res) = &context.root {
            if let Some(root_obj) = root_res.as_json().as_object() {
                if let Some(contained) = root_obj.get("contained").and_then(|v| v.as_array()) {
                    for item in contained {
                        let obj = item.as_object()?;
                        if obj.get("id")?.as_str()? == id {
                            return Some(FhirPathValue::Resource(FhirResource::from_json(
                                item.clone(),
                            )));
                        }
                    }
                }
            }
        }
        None
    }

    /// Check if a string looks like a FHIR reference
    fn is_fhir_reference(&self, reference: &str) -> bool {
        // Basic checks for FHIR reference patterns
        reference.contains('/') ||                    // Relative reference like "Patient/123"
        reference.starts_with("http://") ||          // Absolute URL
        reference.starts_with("https://") ||         // Absolute HTTPS URL
        reference.starts_with("urn:") // URN format
    }

    /// Create a placeholder resource for testing purposes
    /// In a real implementation, this would fetch the actual resource
    fn create_placeholder_resource(&self, reference: &str) -> Option<FhirPathValue> {
        // For now, return a simple placeholder resource
        // This allows tests to pass while indicating that resolve() is working

        // Extract resource type from reference if possible
        let resource_type = if let Some(slash_pos) = reference.find('/') {
            &reference[..slash_pos]
        } else {
            "Resource" // Default fallback
        };

        // Create a minimal placeholder resource
        let placeholder_json = serde_json::json!({
            "resourceType": resource_type,
            "id": reference.split('/').next_back().unwrap_or("unknown"),
            "_placeholder": true,
            "_originalReference": reference
        });

        let resource = FhirResource::from_json(placeholder_json);
        Some(FhirPathValue::Resource(resource))
    }
}
