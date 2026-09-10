package com.example.orders

/** One line of an order: a product, and how many of it. */
data class LineItem(val sku: String, val quantity: Int) {
    init {
        require(quantity > 0) { "quantity must be positive, got $quantity for '$sku'" }
    }
}

/** An order: its lines, in the order they were given. */
data class Order(val items: List<LineItem>) {
    /** How many units the order holds, over every line. */
    val units: Int
        get() = items.sumOf { it.quantity }

    companion object {
        /** Parse `sku:quantity` pairs, as the command line gives them. */
        @JvmStatic
        fun parse(lines: List<String>): Order = Order(lines.map(::parseLine))

        private fun parseLine(text: String): LineItem {
            val parts = text.split(':', limit = 2)
            require(parts.size == 2) { "expected sku:quantity, got '$text'" }
            val quantity = parts[1].trim().toIntOrNull()
                ?: throw IllegalArgumentException("'${parts[1].trim()}' is not a quantity")
            return LineItem(parts[0].trim(), quantity)
        }
    }
}

/**
 * The order's total in cents, before [LegacyPricing]'s bulk discount.
 *
 * `internal`: the tests can call it, since jrs compiles them as a friend of
 * this module, but nothing outside the project can.
 */
internal fun Order.subtotalCents(catalog: Catalog): Long =
    items.sumOf { catalog.priceCents(it.sku) * it.quantity }
