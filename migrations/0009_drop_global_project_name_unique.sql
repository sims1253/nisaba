-- Project names are free-text display labels; projects are identified by UUID
-- and ownership is tracked via project_memberships (there is no owner column on
-- projects). The original `name UNIQUE` constraint (migration 0001) was GLOBAL
-- across the whole system, so once any user created a project named e.g.
-- "My Paper", no other user could ever reuse that name, and a user could not
-- create two of their own projects with the same display name. Drop the global
-- constraint; uniqueness (if ever desired) should be scoped per owner, not
-- system-wide.
ALTER TABLE projects DROP CONSTRAINT IF EXISTS projects_name_key;
