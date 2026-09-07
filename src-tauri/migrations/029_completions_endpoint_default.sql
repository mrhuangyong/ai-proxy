-- Completions default endpoint no longer carries the version segment
-- (default_path_for_format changed /v1/chat/completions → /chat/completions
-- because gateways use varying prefixes: /v1, /v4, /api/v3, ...). Pin legacy
-- empty-endpoint completions rows to the OLD default so their effective URLs
-- are unchanged; new configurations supply the path explicitly.
UPDATE provider_protocols
SET endpoint_path = '/v1/chat/completions'
WHERE format = 'completions' AND (endpoint_path IS NULL OR endpoint_path = '');

UPDATE providers
SET endpoint_path = '/v1/chat/completions'
WHERE format = 'completions' AND (endpoint_path IS NULL OR endpoint_path = '');
