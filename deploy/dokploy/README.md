# Dokploy deployment

Create a service from the repository-root `Dockerfile`, expose container port
`4000`, attach it to the PostgreSQL service network, and provide all values from
`.env.example` as protected environment variables. The health endpoint is
`/health`. Run migrations as a release job before shifting traffic.