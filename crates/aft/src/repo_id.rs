//! Small utility helpers used across the crate.

/// Split a HuggingFace repo id (e.g. `owner/name`) into its two components.
///
/// Splits on the first `/`; the remainder of the string becomes the name, so
/// repo ids like `owner/name/sub` are accepted as `("owner", "name/sub")`.
///
/// Both components are trimmed before being returned. Returns an error if the
/// repo id contains no `/`, if either trimmed component is empty, or if any
/// component is a path-like value (``.` or `..`) or contains invalid
/// characters (`/`, `\`, or `\0`).
pub fn split_hf_repo_id(repo_id: &str) -> Result<(&str, &str), String> {
    let (owner, name) = repo_id
        .split_once('/')
        .ok_or_else(|| format!("invalid HuggingFace repo id '{repo_id}', expected 'owner/name'"))?;

    let owner = owner.trim();
    let name = name.trim();

    if owner.is_empty() {
        return Err(format!(
            "invalid HuggingFace repo id '{repo_id}': owner is empty"
        ));
    }
    if name.is_empty() {
        return Err(format!(
            "invalid HuggingFace repo id '{repo_id}': name is empty"
        ));
    }

    validate_path_component(owner, repo_id, "owner")?;

    for segment in name.split('/') {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(format!(
                "invalid HuggingFace repo id '{repo_id}': name contains an empty segment"
            ));
        }
        validate_path_component(segment, repo_id, "name segment")?;
    }

    Ok((owner, name))
}

/// Validate a single path component of a HuggingFace repo id.
///
/// Rejects `.` and `..` and invalid characters (`/`, `\`, and NUL). The name is
/// split on `/` before each segment is passed here, so slashes are never valid.
fn validate_path_component(component: &str, repo_id: &str, label: &str) -> Result<(), String> {
    if component == "." || component == ".." {
        return Err(format!(
            "invalid HuggingFace repo id '{repo_id}': {label} cannot be '.' or '..'"
        ));
    }
    if component.contains('/') || component.contains('\\') || component.contains('\0') {
        return Err(format!(
            "invalid HuggingFace repo id '{repo_id}': {label} contains an invalid character"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::split_hf_repo_id;

    #[test]
    fn split_hf_repo_id_success() {
        assert_eq!(
            split_hf_repo_id("Qdrant/all-MiniLM-L6-v2-onnx"),
            Ok(("Qdrant", "all-MiniLM-L6-v2-onnx"))
        );
        assert_eq!(
            split_hf_repo_id("minishlab/potion-code-16M"),
            Ok(("minishlab", "potion-code-16M"))
        );
    }

    #[test]
    fn split_hf_repo_id_missing_slash() {
        assert!(split_hf_repo_id("no-slash").is_err());
    }

    #[test]
    fn split_hf_repo_id_multiple_slashes() {
        assert_eq!(split_hf_repo_id("a/b/c"), Ok(("a", "b/c")));
    }

    #[test]
    fn split_hf_repo_id_empty_owner() {
        assert!(split_hf_repo_id("/name").is_err());
        assert!(split_hf_repo_id("  /name").is_err());
    }

    #[test]
    fn split_hf_repo_id_empty_name() {
        assert!(split_hf_repo_id("owner/").is_err());
        assert!(split_hf_repo_id("owner/  ").is_err());
    }

    #[test]
    fn split_hf_repo_id_rejects_dot_components() {
        assert!(split_hf_repo_id("./name").is_err());
        assert!(split_hf_repo_id("owner/.").is_err());
        assert!(split_hf_repo_id("owner/..").is_err());
        assert!(split_hf_repo_id("../name").is_err());
        assert!(split_hf_repo_id("owner/foo/..").is_err());
        assert!(split_hf_repo_id("owner/foo/.").is_err());
        assert!(split_hf_repo_id("owner/foo/.. ").is_err());
        assert!(split_hf_repo_id("owner/foo//bar").is_err());
    }

    #[test]
    fn split_hf_repo_id_rejects_path_separators() {
        assert!(split_hf_repo_id("owner/name/sub").is_ok());
        assert!(split_hf_repo_id("owner/name\\sub").is_err());
        assert!(split_hf_repo_id("owner\\name").is_err());
        assert!(split_hf_repo_id("owner/foo/bar\\baz").is_err());
    }
}
