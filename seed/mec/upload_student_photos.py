"""Upload prepared MEC photographs and attach them to Student Master records.

Run this after applying 01_control.sql and 02_campus.sql.  The API stores every
image in the tenant's Cloudinary folder and synchronises the returned URL to the
student's campus record and login identity, so web and mobile use the same face.
"""

from __future__ import annotations

import argparse
import csv
import getpass
import hashlib
import json
import mimetypes
import os
import time
import uuid
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import unquote, urlparse
from urllib.request import Request, urlopen


SOURCE_DIR = Path(__file__).with_name("source")
MEC_NAMESPACE = uuid.UUID("6f2d1c40-0000-4000-8000-000000000000")


def request_json(
    url: str,
    *,
    method: str = "GET",
    token: str | None = None,
    tenant: str = "mec",
    body: dict | None = None,
    raw_body: bytes | None = None,
    content_type: str = "application/json",
) -> dict:
    headers = {"x-tenant-id": tenant, "x-client-surface": "website"}
    if token:
        headers["authorization"] = f"Bearer {token}"
    data = raw_body
    if body is not None:
        data = json.dumps(body).encode("utf-8")
    if data is not None:
        headers["content-type"] = content_type
    try:
        with urlopen(Request(url, data=data, headers=headers, method=method), timeout=60) as response:
            return json.load(response)
    except HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"{method} {url} failed ({error.code}): {detail}") from error


def multipart(image: Path) -> tuple[bytes, str]:
    boundary = f"----supercampus-{uuid.uuid4().hex}"
    content_type = mimetypes.guess_type(image.name)[0] or "application/octet-stream"
    data = image.read_bytes()
    body = b"".join(
        [
            f"--{boundary}\r\n".encode(),
            f'Content-Disposition: form-data; name="file"; filename="{image.name}"\r\n'.encode(),
            f"Content-Type: {content_type}\r\n\r\n".encode(),
            data,
            f"\r\n--{boundary}--\r\n".encode(),
        ]
    )
    return body, f"multipart/form-data; boundary={boundary}"


def cloudinary_upload(image: Path, tenant: str, cloudinary_url: str) -> str:
    parsed = urlparse(cloudinary_url)
    if parsed.scheme != "cloudinary" or not parsed.username or not parsed.password or not parsed.hostname:
        raise RuntimeError("CLOUDINARY_URL is invalid")
    api_key = unquote(parsed.username)
    api_secret = unquote(parsed.password)
    cloud_name = unquote(parsed.hostname)
    timestamp = str(int(time.time()))
    folder = f"supercampus/{tenant}/media"
    allowed_formats = "jpg,jpeg,png,gif,webp,pdf"
    signed = (
        f"allowed_formats={allowed_formats}&folder={folder}&timestamp={timestamp}{api_secret}"
    )
    signature = hashlib.sha1(signed.encode("utf-8")).hexdigest()
    boundary = f"----supercampus-{uuid.uuid4().hex}"
    parts: list[bytes] = []
    for name, value in {
        "api_key": api_key,
        "timestamp": timestamp,
        "folder": folder,
        "allowed_formats": allowed_formats,
        "signature": signature,
    }.items():
        parts.extend(
            [
                f"--{boundary}\r\n".encode(),
                f'Content-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode(),
            ]
        )
    content_type = mimetypes.guess_type(image.name)[0] or "application/octet-stream"
    parts.extend(
        [
            f"--{boundary}\r\n".encode(),
            f'Content-Disposition: form-data; name="file"; filename="{image.name}"\r\n'.encode(),
            f"Content-Type: {content_type}\r\n\r\n".encode(),
            image.read_bytes(),
            f"\r\n--{boundary}--\r\n".encode(),
        ]
    )
    request = Request(
        f"https://api.cloudinary.com/v1_1/{cloud_name}/auto/upload",
        data=b"".join(parts),
        headers={"content-type": f"multipart/form-data; boundary={boundary}"},
        method="POST",
    )
    with urlopen(request, timeout=60) as response:
        uploaded = json.load(response)
    secure_url = uploaded.get("secure_url", "")
    public_id = uploaded.get("public_id", "")
    if not secure_url.startswith("https://") or not public_id.startswith(folder + "/"):
        raise RuntimeError("Cloudinary returned an invalid tenant media reference")
    return secure_url


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--api-base", default=os.getenv("SUPERCAMPUS_API_BASE_URL"))
    parser.add_argument("--tenant", default="mec")
    parser.add_argument("--email", default="admin@mec.local")
    parser.add_argument("--password", default=os.getenv("MEC_SEED_PASSWORD"))
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    if not args.api_base:
        raise SystemExit("--api-base or SUPERCAMPUS_API_BASE_URL is required")
    password = args.password or getpass.getpass("MEC administrator password: ")
    origin = args.api_base.rstrip("/")
    cloudinary_url = os.getenv("CLOUDINARY_URL")

    try:
        login = request_json(
            origin + "/api/auth/login",
            method="POST",
            tenant=args.tenant,
            body={"email": args.email, "password": password, "sessionMode": "token"},
        )
    except RuntimeError as error:
        if "unknown field `sessionMode`" not in str(error):
            raise
        login = request_json(
            origin + "/api/auth/login",
            method="POST",
            tenant=args.tenant,
            body={"email": args.email, "password": password},
        )
    token = login["data"]["accessToken"]
    response = request_json(
        origin + "/api/v1/student-master", token=token, tenant=args.tenant
    )
    students = {row["rollNo"]: row for row in response["data"]}
    with (SOURCE_DIR / "students.csv").open("r", encoding="utf-8-sig", newline="") as source:
        emails = {row["Register No"]: row["email"].strip().lower() for row in csv.DictReader(source)}

    images = sorted((SOURCE_DIR / "student-images").glob("MEC25*.*"))
    missing_records = [image.stem for image in images if image.stem not in students]
    if missing_records:
        raise SystemExit("Student Master is missing: " + ", ".join(missing_records))

    uploaded = skipped = 0
    photo_rows: list[tuple[str, str, str]] = []
    for index, image in enumerate(images, start=1):
        student = students[image.stem]
        if student.get("photoUrl") and not args.force:
            skipped += 1
            continue
        if cloudinary_url:
            secure_url = cloudinary_upload(image, args.tenant, cloudinary_url)
        else:
            body, content_type = multipart(image)
            media = request_json(
                origin + "/api/v1/media/upload",
                method="POST",
                token=token,
                tenant=args.tenant,
                raw_body=body,
                content_type=content_type,
            )
            secure_url = media["data"]["secureUrl"]
        try:
            request_json(
                origin + f"/api/v1/student-master/{student['id']}/photo",
                method="PUT",
                token=token,
                tenant=args.tenant,
                body={"photoUrl": secure_url},
            )
        except RuntimeError as error:
            # Older deployed APIs can list Student Master but predate the photo
            # attach route.  Keep an auditable SQL handoff for those versions.
            if "failed (404)" not in str(error):
                raise
        user_id = student.get("userId") or str(
            uuid.uuid5(MEC_NAMESPACE, "user|" + emails[image.stem])
        )
        photo_rows.append((image.stem, user_id, secure_url))
        uploaded += 1
        if uploaded % 20 == 0 or index == len(images):
            print(f"Processed {index}/{len(images)} photographs")

    generated = SOURCE_DIR / "generated"
    generated.mkdir(parents=True, exist_ok=True)
    quote = lambda value: "'" + value.replace("'", "''") + "'"
    campus_sql = ["\\set ON_ERROR_STOP on", "BEGIN;"]
    control_sql = ["\\set ON_ERROR_STOP on", "BEGIN;"]
    for roll, user_id, secure_url in photo_rows:
        campus_sql.append(
            "UPDATE core.students SET profile = jsonb_set(COALESCE(profile, '{}'::jsonb), "
            f"'{{photoUrl}}', to_jsonb({quote(secure_url)}::text), true), updated_at = now() "
            f"WHERE student_number = {quote(roll)};"
        )
        control_sql.append(
            "UPDATE identity.users SET profile = jsonb_set(COALESCE(profile, '{}'::jsonb), "
            f"'{{photoUrl}}', to_jsonb({quote(secure_url)}::text), true), updated_at = now() "
            f"WHERE id = {quote(user_id)}::uuid;"
        )
        control_sql.append(
            "UPDATE identity.tenant_memberships membership SET profile = "
            "jsonb_set(COALESCE(membership.profile, '{}'::jsonb), '{photoUrl}', "
            f"to_jsonb({quote(secure_url)}::text), true), updated_at = now() "
            "FROM platform.tenants tenant WHERE tenant.id = membership.tenant_id "
            f"AND tenant.slug = {quote(args.tenant)} AND membership.user_id = {quote(user_id)}::uuid;"
        )
    campus_sql.append("COMMIT;")
    control_sql.append("COMMIT;")
    (generated / "03_photos_campus.sql").write_text("\n".join(campus_sql) + "\n", encoding="utf-8")
    (generated / "04_photos_control.sql").write_text("\n".join(control_sql) + "\n", encoding="utf-8")
    print(f"Photographs uploaded: {uploaded}; already present: {skipped}")
    print(f"Prepared database attachment SQL for {len(photo_rows)} photographs")


if __name__ == "__main__":
    main()
