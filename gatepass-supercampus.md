# SuperCampus – Combined Gate Pass & Security Management Flow

This document merges two source flows into a single reference:
1. **Gate Pass Management** — how passes/QRs are requested, approved, and generated for Students, Staff, and Visitors.
2. **Security Management** — how the gate/security side scans, validates, logs, and (when needed) manually overrides entry/exit.

No steps, labels, or branches have been added beyond what exists in the two source diagrams; this file only restructures and cross-links them.

---

## 1. Top-Level Structure 

```
GATEPASS MANAGEMENT
├── STUDENTS
│   ├── DAY SCHOLARS
│   └── HOSTELLERS
├── STAFF
│   ├── TEACHING STAFF
│   └── NON-TEACHING STAFF
└── VISITORS
    ├── INVITED VISITOR
    └── WALK-INS

SECURITY MANAGEMENT
├── SCAN QR (PRE-APPROVED PASS)
├── WALK-IN VISITOR
└── MANUAL OVERRIDE (SYSTEM DOWN)
```

---

## 2. GATEPASS MANAGEMENT

### 2.1 Students

#### 2.1.1 Day Scholars
- **Walk-Ins → Entry**
  - Entry → Display Generated QR → Scan to Validate
    - **YES** → Allow
    - **NO** → Dont Allow 
- **Day Scholars After Clg Hours** → No Outpass Required  

#### 2.1.2 Hostellers
- **Exit** → Apply Outpass
  - Select Out Pass Type & Student Type,from time, back-in time,submit, Get Approval [Based on Approval Matrix](parents and warden?)
    - **Approval?**
      - **YES** → Generate QR → Scan at Gate and Leave → Notification to Parents Using Geofencing: Left Campus
      - **NO** → Status: Pending / Rejected

#### 2.1.3 Approval Matrix (Hosteller Outpass)

Applies to Hostellers only (Day Scholars require No Outpass, per 2.1.1). Approval is a **sequential chain** — the request must pass each approver in order; a rejection at any stage sets the status to **Rejected**, and passing every stage in the chain sets it to **Approved → Generate QR**.

| Outpass Type | Step 1 | Step 2 | Step 3 | Step 4 |
|---|---|---|---|---|
| Day Out | Class Advisor | Warden | — | — |
| Home Visit | Class Advisor | Head of Department | Warden | Principal |
| Medical | Class Advisor | Warden | Admin | — |
| Emergency | Warden | Admin | — | — |

summary-> class advisors/hod for day scholars and warden for hostellers 

### 2.2 Staff

- **Entry**
  - Entry → Scan the Generated QR
    - **VALID** → Allow
    - **NOT VALID** → Dont Allow

- **Exit**
  - Exit → Early Exit / Exit During Working Hours?
    - **YES** → Apply for Exit Permission
      - HOD / Principal [Teaching Staff] Approval
      - Admin [Non Teaching Staff] Approval
      - → **Approved?**
        - **YES** → Exit QR Generated → Scan and Leave
    - **NO** → Regular Exit (After Working Hours)

*(Entry and Exit are the shared processes below Staff, applicable to Teaching Staff and Non-Teaching Staff; approval routing at the Exit step splits by staff type as shown above.)*

### 2.3 Visitors

#### 2.3.1 Invited Visitor
- Verify Visitor Details with Pre-Approved Details Sent by Admin
  - **VERIFIED** → QR Sent via WhatsApp → Scan and Verify and Enter → Scan Again During Exit
  - **IF DETAILS NOT FOUND** → Verify with Admin and Request Data and Approval
    - **VERIFIED** → QR Sent via WhatsApp
    - **NOT VERIFIED** → Do Not Allow

#### 2.3.2 Walk-Ins
- Enter Details + Purpose + Whom to Meet via Security's Device
  - Capture Photo
    - Request Approval
      - **APPROVAL? YES** → QR Sent via WhatsApp (Based on Duration) → Scan and Enter
        - Notification Sent Before 15min of QR Expiry
        - Scan and Exit Before QR Expires
      - **APPROVAL? NO** → Not Allowed

---

## 3. SECURITY MANAGEMENT

### 3.1 Scan QR (Pre-Approved Pass)
- Scan QR (Pre-Approved Pass) → Validate QR (Online or Cached Token)
  - **VALID?**
    - **YES** → Log Entry / Exit → Allow
    - **NO** → Flag Incident → Deny

### 3.2 Walk-In Visitor
- Walk-In Visitor → Capture Details + Photo (Purpose, Whom to Meet) → Request Approval (Sent to Host / Admin)
  - **APPROVED?**
    - **YES** → Generate Onsite QR (Time-Boxed) → Scan and Allow Entry → Notification Sent Before QR Expiry → Scan and Exit Before Expiry
    - **NO** → Not Allowed

### 3.3 Manual Override (System Down)
- Manual Override (System Down) → Record ID + Reason (Photo if Possible) → Allow + Flag for Review → Admin Reviews Later

---

## 4. How the Two Modules Connect

- Every QR produced anywhere in **Section 2 (Gate Pass Management)** — Day Scholar entry QR, Hosteller Exit QR, Staff Entry/Exit QR, Invited Visitor QR, Walk-In Visitor QR — is what gets presented and processed at the gate in **Section 3.1 (Scan QR – Pre-Approved Pass)**.
- The **Walk-Ins** journey appears on both sides:
  - Section 2.3.2 describes it from the pass-issuance side (visitor enters details on security's device, photo captured, approval requested, QR sent via WhatsApp based on duration).
  - Section 3.2 describes the same journey from the security/gate-operations side (capture details + photo, request approval sent to host/admin, generate onsite time-boxed QR, scan in/out before expiry).
- **Manual Override (Section 3.3)** is the fallback path used at the gate for any actor/pass type only when the QR system itself is unavailable — entry/exit is allowed and flagged for the admin to review afterward.

---

## 5. Actor / Decision Reference (for backend state modeling)

| Actor | Pass Type | Trigger | Approver | Output | Gate Action |
|---|---|---|---|---|---|
| Day Scholar | Entry pass | Walk-in at entry | — | Generated QR displayed | Scan to Validate → Allow / Dont Allow |
| Day Scholar (after college hours) | — | — | — | No Outpass Required | — |
| Hosteller | Outpass | Apply Outpass | Sequential chain per Approval Matrix (see 2.1.3): Class Advisor / HOD / Warden / Principal / Admin depending on Outpass Type | Generate QR (if all approvers = Yes) / Pending-Rejected (if any = No) | Scan at Gate and Leave → Notification to Parents (Geofencing: Left Campus) |
| Teaching Staff | Entry pass | Entry | — | Generated QR | Scan the Generated QR → Valid/Not Valid → Allow/Dont Allow |
| Teaching Staff | Exit permission | Early/Working-hours Exit | HOD / Principal | Exit QR Generated (if Approved) | Scan and Leave |
| Non-Teaching Staff | Exit permission | Early/Working-hours Exit | Admin | Exit QR Generated (if Approved) | Scan and Leave |
| Staff (either) | Regular Exit | Exit after working hours | — | — | Regular Exit (no permission flow) |
| Invited Visitor | Pre-approved QR | Details verified against pre-approved list | Admin (if not found) | QR Sent via WhatsApp / Do Not Allow | Scan and Verify and Enter → Scan Again During Exit |
| Walk-In Visitor | Onsite QR | Enter details + purpose + photo | Host/Admin approval | QR Sent via WhatsApp based on duration (if Approved) / Not Allowed | Scan and Enter → Notification before 15min expiry → Scan and Exit before expiry |
| Any pre-approved pass holder | — | Scan QR at gate | — | Validate QR (online/cached token) | Valid → Log Entry/Exit → Allow; Not Valid → Flag Incident → Deny |
| Any actor (system down) | — | Manual check-in | — | Record ID + Reason (+ photo) | Allow + Flag for Review → Admin Reviews Later |
