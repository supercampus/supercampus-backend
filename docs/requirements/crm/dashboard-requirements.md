# SuperCampus CRM — Unified Dashboard Specification

## 1. Dashboard Philosophy

The Unified Dashboard is a single-screen Kanban board. It is the only screen users interact with. There are no metrics carousels, no bottom widgets, no notification panels, and no sound alerts. The design is intentionally minimal and focused entirely on the pipeline.

The visual reference is a clean, dark-themed Kanban board with icon-based sidebar navigation, a top control bar, columns, and cards with label tags.

---

## 2. Global Layout

```
+----------------------------------------------------------------------------------+
|  TOP BAR                                                                         |
|  [Logo]  Board Title              Breadcrumb    [Visibility] [Filter] [+ New List]|
+----------------------------------------------------------------------------------+
|                                                                                  |
|  +--+                                                                            |
|  |  |  LEFT SIDEBAR (Icon Only)                                                  |
|  |IC|  +----+                                                                   |
|  |ON|  |Icon|  Boards                                                            |
|  |  |  +----+                                                                   |
|  |NA|  |Icon|  Search                                                            |
|  |V |  +----+                                                                   |
|  |  |  |Icon|  Forms                                                             |
|  +--+  +----+                                                                   |
|  |Icon|  Communications                                                         |
|  +----+                                                                   |
|  |Icon|  Users (Manager only)                                                    |
|  +----+                                                                   |
|  |Icon|  Settings (Manager only)                                                 |
|  +----+                                                                   |
|                                                                                  |
|  MAIN CONTENT: KANBAN BOARD ONLY                                                 |
|  +----------+ +----------+ +----------+ +----------+ +----------+               |
|  | ENQUIRY  | | CONTACT  | |CONTACTED | | NURTURE  | | QUALIFIED|               |
|  |   (24)   | | ATTEMPTED| |   (18)   | |   (31)   | |   (22)   |               |
|  |          | |   (12)   | |          | |          | |          |               |
|  | [Card]   | | [Card]   | | [Card]   | | [Card]   | | [Card]   |               |
|  | [Card]   | | [Card]   | | [Card]   | | [Card]   | | [Card]   |               |
|  | [Card]   | |          | | [Card]   | | [Card]   | |          |               |
|  +----------+ +----------+ +----------+ +----------+ +----------+               |
|  +----------+ +----------+ +----------+                                         |
|  |APPLICATION| | APP      | | OFFER    |                                         |
|  |   (19)   | | STATUS   | | STATUS   |                                         |
|  |          | |   (14)   | |   (8)    |                                         |
|  | [Card]   | | [Card]   | | [Card]   |                                         |
|  | [Card]   | | [Card]   | | [Card]   |                                         |
|  +----------+ +----------+ +----------+                                         |
|                                                                                  |
+----------------------------------------------------------------------------------+
```

---

## 3. Top Bar

### 3.1 Elements

| Element | Position | Behavior |
|---------|----------|----------|
| Logo | Far left | Click returns to default board view |
| Board Title | Left of center | Current board name (e.g., "Pre-Admission Pipeline") |
| Breadcrumb | Center | Organization / Board / View path (e.g., "SuperCampus / CRM / Pre-Admission") |
| Visibility | Right | Toggle between Private, Team, and Public board visibility |
| Filter | Right | Open filter panel for source, stage, assignee, date range |
| + New List | Far right | Add a new column to the board (Manager only) |

### 3.2 Filter Panel

When Filter is clicked, a dropdown or side panel opens with:
- Lead Source (multi-select dropdown of all 35 sources)
- Assigned To (multi-select of counselors)
- Stage Duration (slider: 0 to 30+ days)
- Date Range (created from, created to)
- Program (multi-select)
- Priority (High, Medium, Low)
- Global Status (Prospect, Deferred, On Hold, Archive)

Applying filters updates the board in real time. A "Clear All" button resets filters.

---

## 4. Left Sidebar

### 4.1 Icon Navigation

The sidebar contains icon-only navigation. Icons are monochrome. Active icon is highlighted.

| Icon | Label (tooltip on hover) | Visible To |
|------|--------------------------|------------|
| Boards | My Boards | All |
| Search | Global Search | All |
| Forms | Form Builder | All (edit restricted by role) |
| Communications | Communication Log | All |
| Users | Team Management | Manager only |
| Settings | Board Settings | Manager only |

### 4.2 Sidebar Behavior

- Collapsible: Click toggle to collapse to 48px width
- Hover on collapsed icon shows tooltip with label
- Active state: icon background highlight
- Unavailable items for non-managers are hidden, not disabled

---

## 5. Kanban Board

### 5.1 Column Specifications

| Column | Width | Color Accent |
|--------|-------|--------------|
| Enquiry | 280px | Neutral gray |
| Contact Attempted | 280px | Light blue |
| Contacted | 280px | Blue |
| Nurture | 280px | Indigo |
| Qualified | 280px | Purple |
| Application | 320px | Orange |
| Application Status | 320px | Amber |
| Offer Status | 280px | Green |
| Archived | 240px | Red |

Columns are horizontally scrollable. Each column has independent vertical scroll.

### 5.2 Column Header

```
+--------------------------------+
|  STAGE NAME           [+] [...]|  <- Stage name, Add card, Column menu
|  ------------------------------|
|  24 leads                      |  <- Lead count
+--------------------------------+
```

Header elements:
- Stage name in bold
- Add card button (+): Click creates new card at top of column
- Column menu (...): Sort, filter, collapse, archive all (manager only)
- Lead count below header line

### 5.3 Column Menu Options

| Option | Available To |
|--------|--------------|
| Sort by Name | All |
| Sort by Date Added | All |
| Sort by Follow-up Date | All |
| Sort by Priority | All |
| Filter Column | All |
| Collapse Column | All |
| Archive All Cards | Manager only |
| Rename Column | Manager only |
| Delete Column | Manager only |

### 5.4 Card Design

```
+---------------------------+
|  Rahul Sharma             |  <- Student name (bold)
|  +91 98XXX 45XXX          |  <- Masked phone (gray text)
|                           |
|  [● Google Ads]           |  <- Label tag: colored dot + source name
|                           |
|  Assigned: Anjali         |  <- Counselor name
|  Follow-up: Today         |  <- Next action date
|  [0/4]                    |  <- Sub-task progress (if applicable)
+---------------------------+
```

Card elements:
- Student name (bold, clickable to open detail view)
- Masked phone number
- Label tag: colored dot + lead source name (e.g., blue dot "Google Ads", orange dot "Walk-In")
- Assigned counselor name
- Next follow-up date (red if overdue)
- Sub-task progress indicator (e.g., "2/5 docs" or "1/3 interviews")
- Hover state: subtle elevation shadow

### 5.5 Card Labels

Label tags use the following color mapping:

| Source Category | Dot Color |
|-----------------|-----------|
| Search Engines | Blue |
| Social Media | Pink |
| Education Portals | Purple |
| Referrals | Green |
| Walk-In / Events | Orange |
| Agent/Consultant | Yellow |
| Broadcast / Call | Teal |
| Other | Gray |

### 5.6 Card Actions

Right-click on card opens context menu:
- Open Detail
- Move to Stage (submenu of all stages)
- Assign To (submenu of counselors)
- Set Priority (High, Medium, Low)
- Set Follow-up Date
- Send WhatsApp
- Log Call
- Place On Hold
- Flag for Archive
- Archive (Manager only)

### 5.7 Drag and Drop

Drag Start:
- Card lifts with shadow
- Invalid drop columns gray out
- Valid drop columns highlight with border

Drag Over Valid Column:
- Column header pulses subtly
- Drop zone indicator appears between cards

Drop:
- Card settles into new position
- Column counters update
- Toast confirmation: "Rahul Sharma moved to Qualified"
- If WhatsApp triggered: "Notification queued"

Drop Invalid:
- Card snaps back
- Error toast: "Permission denied for this stage transition"

---

## 6. Card Detail View (Modal)

Clicking a card opens a side panel or modal with full lead details.

```
+--------------------------------------------------+
|  Rahul Sharma                          [X]        |
|  +91 98XXX 45XXX | rahul@email.com               |
|  ------------------------------------------------|
|  Source: Google Ads                               |
|  Assigned to: Anjali                              |
|  Stage: Qualified                                 |
|  Days in stage: 5                                 |
|  ------------------------------------------------|
|  TABS: [Details] [Timeline] [Documents] [Forms]  |
|                                                   |
|  Details Tab:                                     |
|  Name: Rahul Sharma                               |
|  Phone: +91 98XXX 45XXX                           |
|  Email: rahul@email.com                           |
|  Parent: Mr. Sharma | +91 99XXX 12XXX             |
|  Program: B.Tech CSE                              |
|  Intake: 2026                                     |
|  ------------------------------------------------|
|  Academic: 12th CBSE | 85% | 2025                |
|  ------------------------------------------------|
|  [Edit] [Move Stage] [Send Message] [Archive]    |
+--------------------------------------------------+
```

Tabs:
- Details: All lead fields, editable per role permissions
- Timeline: Stage history, communication log, activity feed
- Documents: List of uploaded documents with verification status
- Forms: Linked form submissions for this lead

---

## 7. Role-Based Board Views

### 7.1 Admission Counselor View

Visible:
- All 9 columns
- Only cards assigned to this counselor
- Can drag cards within permitted stage range
- Can edit lead details on assigned cards
- Can log calls and send messages
- Can place On Hold
- Can flag for archive

Hidden:
- Team Management icon
- Settings icon
- Archive All option in column menu
- Delete Column option

### 7.2 Marketing Team View

Visible:
- All 9 columns
- All cards (read-only)
- Cannot drag cards
- Cannot edit leads
- Can view Forms and Communications (read-only)

Hidden:
- Add card button
- Column menu (except Sort and Filter)
- Card context menu actions
- Team Management and Settings icons

### 7.3 Manager View

Visible:
- All 9 columns
- All cards
- Full drag and drop across all stages
- Full edit on all leads
- All sidebar icons including Team Management and Settings
- All column menu options
- Can add, rename, delete columns
- Can archive any card

---

## 8. Real-Time Updates

WebSocket events update the board without page refresh:

| Event | UI Update |
|-------|-----------|
| lead.created | New card slides into Enquiry column |
| lead.moved | Card animates from source to target column |
| lead.assigned | Card updates assigned name |
| lead.archived | Card fades out of active board |
| stage.count_changed | Column counter updates |

No sound alerts. No browser notifications. Visual updates only.

---

## 9. Search

### 9.1 Global Search

Activated by clicking the Search icon in the sidebar or pressing Ctrl+K.

```
+--------------------------------------------------+
|  Search leads, students, applications...          |
+--------------------------------------------------+
```

Searchable fields:
- Student name
- Phone number
- Email
- Lead ID
- Assigned counselor name

Results appear as a dropdown list. Clicking a result opens the card detail view.

---

## 10. Responsive Behavior

| Breakpoint | Layout |
|------------|--------|
| Desktop (>1200px) | Full sidebar + all columns visible |
| Tablet (768-1200px) | Collapsed sidebar + horizontal scroll |
| Mobile (<768px) | Bottom nav replaces sidebar + swipeable columns |

---

## 11. Performance Requirements

| Metric | Target |
|--------|--------|
| Initial Board Load | Under 2 seconds |
| Card Render | Under 100ms per card |
| Drag and Drop Response | Under 50ms |
| Real-time Event Latency | Under 200ms |
| Search Response | Under 300ms |
| Filter Application | Under 100ms |
| Concurrent Users | 500+ without degradation |

---

## 12. Accessibility

- WCAG 2.1 AA compliance
- Keyboard navigation for all actions
- Screen reader support for column headers and card content
- High contrast mode toggle
- Font size adjustment (100% to 200%)
- Color-blind friendly labels (icons accompany colors)
