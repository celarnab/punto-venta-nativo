-- Permisos extra que un administrador le da a un cajero. Un administrador ya puede todo, así que los suyos quedan
-- en 0.
ALTER TABLE usuarios ADD COLUMN ver_reportes       INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usuarios ADD COLUMN manejar_productos  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usuarios ADD COLUMN manejar_inventario INTEGER NOT NULL DEFAULT 0;
