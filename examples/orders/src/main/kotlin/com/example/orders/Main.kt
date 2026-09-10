package com.example.orders

import kotlin.system.exitProcess
import kotlinx.coroutines.runBlocking

private val SAMPLE = listOf("apple:12", "bread:1", "coffee:2")

fun main(args: Array<String>) {
    val order = try {
        Order.parse(args.toList().ifEmpty { SAMPLE })
    } catch (e: IllegalArgumentException) {
        System.err.println("orders: ${e.message}")
        exitProcess(2)
    }
    val catalog = Catalog.DEFAULT
    val lines = runBlocking { catalog.quote(order) }
    for ((item, cents) in order.items.zip(lines)) {
        println("%-8s x%-3d %8s".format(item.sku, item.quantity, LegacyPricing.format(cents)))
    }
    println(LegacyPricing.receipt(order, order.subtotalCents(catalog)))
}
