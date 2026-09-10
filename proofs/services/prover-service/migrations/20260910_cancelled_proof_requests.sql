ALTER TABLE proof_requests DROP CONSTRAINT proof_requests_status_check;
ALTER TABLE proof_requests ADD CONSTRAINT proof_requests_status_check
    CHECK (proof_status IN ('CREATED', 'RUNNING', 'SUCCEEDED', 'FAILED', 'CANCELLED'));
