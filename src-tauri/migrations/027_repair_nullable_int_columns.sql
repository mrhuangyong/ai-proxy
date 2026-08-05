-- Migration 027: repair backups that flattened NULL → "" in nullable INTEGER
-- columns.
--
-- Pre-fix export wrote an empty string (not NULL) for NULL values. Importing
-- such a bundle bound "" into e.g. provider_models.max_output_tokens, and
-- SQLite stored it as TEXT '' (it only converts numeric-looking text to the
-- column's INTEGER affinity). Reading the column back as Option<i64> then
-- failed with "mismatched types ... INTEGER ... not compatible with SQL type
-- TEXT". These repairs normalize the corrupted values back to NULL.

UPDATE provider_models SET max_output_tokens = NULL
  WHERE typeof(max_output_tokens) = 'text';

UPDATE provider_models SET context_window = NULL
  WHERE typeof(context_window) = 'text';
