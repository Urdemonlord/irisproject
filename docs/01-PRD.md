# IRIS Product Requirements Document (PRD)

**Product:** IRIS\
**Parent brand:** Meow Labs\
**Primary application URL:** `https://mail.meowlabs.id`\
**Status:** Draft v1.0\
**Date:** 30 October 2026

## 1. Product overview

IRIS is a self-hosted email platform from Meow Labs that combines two
experiences in one web application:

1.  **Permanent mailboxes** for authorized personal and business use,
    including receiving, reading, composing, replying to, and organizing
    email.
2.  **Temporary inboxes** for short-lived email reception, such as
    testing integrations or receiving verification messages where
    permitted.

IRIS is an application and user experience layer. A mature mail server
handles email protocols and delivery. The application backend is written
in Rust.

**Product statement:** IRIS gives users a simple, controlled place to
manage permanent and temporary email without requiring Meow Labs to
implement SMTP or IMAP from scratch.

**Tagline candidate:** Every message finds its way.

## 2. Problem statement

Meow Labs needs a dependable web interface for domain email after the
previous mail service expired. The product should restore a business
mailbox experience while providing an isolated temporary-inbox feature
that can later be offered to other users.

## 3. Goals

-   Provide a reliable webmail interface at `mail.meowlabs.id`.
-   Support permanent mailboxes on `meowlabs.id`.
-   Support temporary addresses on a separate domain, proposed as
    `inbox.meowlabs.id`.
-   Use Rust for application APIs and business logic.
-   Use a mature mail server rather than implementing SMTP/IMAP
    protocols.
-   Protect permanent email from anonymous temporary-inbox users.
-   Apply explicit quotas, expiration rules, and abuse controls.
-   Keep the first release small enough for a single developer to
    operate.

## 4. Non-goals for MVP

-   Replacing Gmail or Outlook feature-for-feature.
-   End-to-end encrypted email.
-   Providing anonymous outbound email.
-   Sending bulk email or marketing campaigns.
-   Public disposable inboxes for bypassing third-party restrictions.
-   Building a custom SMTP or IMAP server.
-   Mobile native applications.
-   Multi-region replication or enterprise compliance certifications.

## 5. Target users and use cases

### Primary users

-   Meow Labs staff or owner using permanent business mailboxes.
-   Authorized users who need a straightforward browser-based email
    client.
-   Developers who need temporary inboxes for legitimate testing.

### Core use cases

-   Sign in and read business email.
-   Search and open a message.
-   Compose, reply to, and forward email.
-   Manage aliases and folders where supported.
-   Create a temporary inbox and copy its address.
-   Observe incoming messages in a temporary inbox.
-   Allow an inbox to expire and delete its data according to retention
    policy.
-   Administrators monitor delivery health, storage, and abuse.

## 6. Product modes

### 6.1 Permanent mailbox

-   Requires authentication.
-   Access is limited to the mailbox owner and explicitly authorized
    administrators.
-   Supports receiving, reading, composing, replying, forwarding,
    searching, and attachments.
-   Uses the `meowlabs.id` domain.
-   Retention is governed by mailbox policy, not temporary-inbox expiry.

### 6.2 Temporary inbox

-   Anonymous creation may be enabled only after rate limits and abuse
    controls are implemented.
-   Uses a separate domain, proposed as `inbox.meowlabs.id`.
-   Generates cryptographically random local parts.
-   Does not permit outbound email in MVP.
-   Has a configurable expiry; initial proposal is 1 hour, with an
    optional extension.
-   Expired inboxes and their messages are deleted by a scheduled
    cleanup job, subject to a short operational deletion window.
-   Public inbox addresses must not expose permanent mailbox data or
    internal mail-server credentials.

## 7. Functional requirements

  -----------------------------------------------------------------------
  ID                      Requirement             Priority
  ----------------------- ----------------------- -----------------------
  FR-01                   Authenticate permanent  P0
                          mailbox users           

  FR-02                   Show inbox message list P0
                          with sender, subject,   
                          date, and read state    

  FR-03                   Open and safely render  P0
                          a message               

  FR-04                   Compose and send mail   P0
                          from authorized         
                          permanent mailboxes     

  FR-05                   Reply to and forward    P1
                          messages                

  FR-06                   Search messages by      P1
                          supported fields        

  FR-07                   Display and download    P0
                          attachments safely      

  FR-08                   Create temporary        P0
                          inboxes with random     
                          addresses               

  FR-09                   Poll or subscribe for   P1
                          new temporary messages  

  FR-10                   Show expiry countdown   P1
                          and allow permitted     
                          extension               

  FR-11                   Delete expired          P0
                          temporary inbox data    

  FR-12                   Enforce per-IP and      P0
                          per-session creation    
                          limits                  

  FR-13                   Provide admin health,   P1
                          queue, storage, and     
                          error views             

  FR-14                   Support account logout  P0
                          and session revocation  

  FR-15                   Record                  P0
                          security-relevant audit 
                          events without logging  
                          message bodies          
  -----------------------------------------------------------------------

## 8. Non-functional requirements

-   **Security:** HTTPS everywhere; secure session cookies; CSRF
    protection where relevant; strict authorization at every mailbox
    boundary.
-   **Privacy:** Do not use message content for analytics or model
    training. Do not log message bodies, passwords, tokens, or
    attachment contents.
-   **Reliability:** Failed mail-server or storage calls should produce
    controlled errors and useful operational logs.
-   **Performance:** Initial message list should be paginated; avoid
    loading entire mailboxes at once.
-   **Accessibility:** Keyboard-operable controls, semantic HTML,
    visible focus, adequate contrast.
-   **Maintainability:** Modular Rust code, migration-managed schema,
    automated tests, documented deployment.
-   **Resource use:** Enforce message size, attachment size, inbox
    count, and storage limits.
-   **Data lifecycle:** Retention and deletion rules must be explicit
    and testable.

## 9. Success metrics

-   Permanent users can sign in and access their inbox successfully.
-   Test messages sent from external providers arrive and display
    correctly.
-   No cross-mailbox access is possible in authorization tests.
-   Temporary inboxes expire and are cleaned up within the configured
    cleanup interval.
-   Storage usage and mail queue health are observable.
-   No critical secrets or email bodies appear in application logs.

## 10. MVP acceptance criteria

1.  A configured permanent account can receive, read, compose, and send
    a test email.
2.  Message HTML is isolated from the application origin or rendered
    with strict sanitization.
3.  Temporary inbox creation returns a high-entropy address and expiry
    timestamp.
4.  A message delivered to a temporary address appears only in that
    inbox.
5.  A temporary inbox cannot access permanent mailboxes, even by
    changing identifiers.
6.  Expired inboxes reject further access and are scheduled for
    deletion.
7.  Rate limits and size limits are enforced server-side.
8.  DNS, TLS, SPF, DKIM, and DMARC are documented and validated before
    production.
9.  Backup and restore procedures are tested before storing important
    business email.

## 11. Risks and mitigations

-   **Spam and abuse:** rate limits, quotas, blocklists, no outbound
    temp mail, admin disable switch.
-   **Deliverability:** use a stable IP, configure reverse DNS, SPF,
    DKIM, DMARC, and monitor reputation.
-   **VPS resource pressure:** limit message sizes and storage; alert
    before disk exhaustion.
-   **Data loss:** backup mail data and configuration; perform restore
    drills.
-   **Malicious email HTML or attachments:** sandbox message rendering,
    force downloads where appropriate, never execute attachments.
-   **Domain confusion:** separate permanent and temporary address
    domains.

## 12. Open decisions

-   Final mail-server version and deployment model.
-   Whether anonymous temporary inbox creation is public at launch or
    invite-only.
-   Exact temporary retention duration and extension limits.
-   Whether temporary inboxes are shareable by secret URL or
    session-bound.
-   Storage quotas and backup retention.
-   Whether permanent accounts are created manually by an administrator
    or through self-service registration.
