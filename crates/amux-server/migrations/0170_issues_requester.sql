-- The human who asked for a card's work. Pushes and pull requests for the card
-- are made with this person's own GitHub sign-in, so the value has to be
-- something nobody can type: it is stamped by the server at create time
-- (api/card_requester.rs) and never accepted from a request body.
--
-- `creator` cannot carry this. When a lead splits a goal into child cards, the
-- children's creator is the lead worker, and the person who asked is lost.
--
-- NULL means nobody is known to have asked: the card's work must not be pushed
-- under anyone's account until a person claims it through a new card.
-- ADDCOL: issues requester TEXT

-- Children made by decompose (or any writer that sets `epic`) take the parent's
-- requester, so every path that files a child is covered, not only the HTTP one.
DROP TRIGGER IF EXISTS issues_requester_from_epic;
CREATE TRIGGER issues_requester_from_epic AFTER INSERT ON issues
WHEN NEW.requester IS NULL AND NEW.epic IS NOT NULL
BEGIN
    UPDATE issues
       SET requester = (SELECT p.requester FROM issues p WHERE p.id = NEW.epic)
     WHERE id = NEW.id;
END;

-- Once set, the requester never changes. A worker that could rewrite it could
-- push under another person's account.
DROP TRIGGER IF EXISTS issues_requester_immutable;
CREATE TRIGGER issues_requester_immutable BEFORE UPDATE OF requester ON issues
WHEN OLD.requester IS NOT NULL AND NEW.requester IS NOT OLD.requester
BEGIN
    SELECT RAISE(ABORT, 'issues.requester is immutable once set');
END;
