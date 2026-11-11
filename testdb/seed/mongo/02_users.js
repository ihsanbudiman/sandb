// sandb test seed (MongoDB 8): users. Runs last, so a TCP login of sandb_ro in the
// healthcheck only passes after the data seed has finished.
// Local test-only passwords (same as .env.test).
const admin = db.getSiblingDB('admin');
admin.createUser({
  user: 'sandb_rw',
  pwd: 'sandb_rw_pw',
  roles: [{ role: 'readWrite', db: 'shop_staging' }, { role: 'readWrite', db: 'analytics' }],
});
admin.createUser({
  user: 'sandb_ro',
  pwd: 'sandb_ro_pw',
  roles: [{ role: 'read', db: 'shop_staging' }, { role: 'read', db: 'analytics' }],
});
