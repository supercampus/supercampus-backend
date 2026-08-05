# Swagger UI and ReDoc

`docs/openapi.yaml` and `docs/swagger.json` are OpenAPI 3.1 documents with identical contracts. The YAML file is intentionally JSON-formatted; JSON is valid YAML.

They can be imported directly into a current Swagger UI, Swagger Editor, ReDoc, Postman or API client generator.

The Rust backend does not currently host `/swagger`, `/docs`, or `/openapi.json`. Hosting an interactive documentation route is **Not Implemented**. Do not expose development examples or secrets when publishing the specification.
