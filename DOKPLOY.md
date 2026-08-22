# Dokploy API deployment

Deploy this repository as the API service and attach only
`api.supercampus.ai` to container port `4000`.

| Dokploy field | Value |
| --- | --- |
| Build Type | `Dockerfile` |
| Docker Context | `.` |
| Dockerfile | `Dockerfile` |
| Container Port | `4000` |
| Health path | `/health` |
| Domain | `api.supercampus.ai` |
| HTTPS | Enabled, with HTTP redirected to HTTPS |

Start from `.env.production.example` and enter real values in Dokploy's
encrypted environment settings. Do not paste those values into source files,
Docker build arguments, screenshots, or frontend variables.

Required production values include `CONTROL_DATABASE_URL`, a unique random
`JWT_SECRET` of at least 32 characters, and the Cloudinary/SMTP credentials used
by the deployment. Keep `SEED_TEST_USERS=false`.

The API validates production security configuration before opening its socket.
It refuses placeholder JWT secrets, HTTP CORS origins, non-origin CORS paths,
test-user seeding, and a non-HTTPS `APP_PUBLIC_URL`.

After deploying, verify:

```powershell
Invoke-RestMethod https://api.supercampus.ai/health
curl.exe -I https://api.supercampus.ai/health
```

The first command must report `status: ok`; the second must show a valid HTTPS
response. Authenticated endpoints should return `401` without a session rather
than exposing data.
