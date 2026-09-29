//! The resident semantic vector arena: vectors decoded once at admission so
//! scoring never decodes SQLite rows, with shared payload counted once.
