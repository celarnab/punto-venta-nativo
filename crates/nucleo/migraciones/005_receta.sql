-- Farmacias: productos que solo se venden con receta médica. Al cobrar una venta que los lleva, el cajero confirma
-- que el cliente presentó una receta válida.

ALTER TABLE productos ADD COLUMN con_receta INTEGER NOT NULL DEFAULT 0;
