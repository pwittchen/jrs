package com.example.orders

import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay

/** Unit prices, in cents. */
class Catalog(private val prices: Map<String, Long>) {
    fun priceCents(sku: String): Long =
        prices[sku] ?: throw IllegalArgumentException("unknown product '$sku'")

    /**
     * Price every line at once, as a slow pricing service would be asked:
     * one coroutine per line.
     */
    suspend fun quote(order: Order): List<Long> = coroutineScope {
        order.items
            .map { item ->
                async {
                    delay(10)
                    priceCents(item.sku) * item.quantity
                }
            }
            .awaitAll()
    }

    companion object {
        val DEFAULT = Catalog(
            mapOf(
                "apple" to 40L,
                "bread" to 250L,
                "cheese" to 890L,
                "coffee" to 1_200L,
            ),
        )
    }
}
