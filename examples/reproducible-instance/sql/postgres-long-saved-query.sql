-- Deliberately long, executable query for editor, outline, completion, and
-- saved-query performance testing against the seeded PostgreSQL lab dataset.
WITH parameters AS (
  SELECT
    timestamptz '2024-01-01 00:00:00+00' AS window_start,
    timestamptz '2025-01-01 00:00:00+00' AS window_end,
    3::integer AS minimum_orders,
    25::integer AS result_limit
),
line_totals AS (
  SELECT
    item.order_id,
    count(*) AS line_count,
    sum(item.quantity) AS units,
    round(
      sum(
        item.quantity
        * item.unit_price
        * (1 - item.discount_percent / 100.0)
      ),
      2
    ) AS calculated_total,
    round(avg(item.discount_percent), 2) AS average_discount_percent,
    count(*) FILTER (
      WHERE item.discount_percent > 0
    ) AS discounted_line_count
  FROM lab.order_items AS item
  GROUP BY item.order_id
),
order_facts AS (
  SELECT
    orders.id AS order_id,
    orders.customer_id,
    orders.salesperson_id,
    orders.status,
    orders.currency,
    orders.placed_at,
    orders.total AS recorded_total,
    coalesce(lines.calculated_total, 0) AS calculated_total,
    coalesce(lines.line_count, 0) AS line_count,
    coalesce(lines.units, 0) AS units,
    coalesce(lines.average_discount_percent, 0) AS average_discount_percent,
    coalesce(lines.discounted_line_count, 0) AS discounted_line_count,
    orders.shipping_address ->> 'country' AS shipping_country,
    orders.shipping_address ->> 'city' AS shipping_city
  FROM lab.orders AS orders
  CROSS JOIN parameters
  LEFT JOIN line_totals AS lines
    ON lines.order_id = orders.id
  WHERE orders.placed_at >= parameters.window_start
    AND orders.placed_at < parameters.window_end
),
customer_rollup AS (
  SELECT
    customer.id AS customer_id,
    customer.company_name,
    customer.region,
    customer.credit_limit,
    customer.tags,
    customer.attributes ->> 'newsletter' AS newsletter_status,
    count(facts.order_id) AS order_count,
    count(facts.order_id) FILTER (
      WHERE facts.status = 'shipped'
    ) AS shipped_order_count,
    count(facts.order_id) FILTER (
      WHERE facts.status = 'cancelled'
    ) AS cancelled_order_count,
    count(DISTINCT facts.salesperson_id) AS salesperson_count,
    count(DISTINCT facts.shipping_country) AS shipping_country_count,
    coalesce(sum(facts.calculated_total), 0) AS gross_revenue,
    coalesce(sum(facts.units), 0) AS units,
    coalesce(sum(facts.line_count), 0) AS line_count,
    coalesce(sum(facts.discounted_line_count), 0) AS discounted_line_count,
    round(coalesce(avg(facts.calculated_total), 0), 2) AS average_order_value,
    round(coalesce(avg(facts.average_discount_percent), 0), 2)
      AS average_discount_percent,
    min(facts.placed_at) AS first_order_at,
    max(facts.placed_at) AS latest_order_at
  FROM lab.customers AS customer
  LEFT JOIN order_facts AS facts
    ON facts.customer_id = customer.id
  GROUP BY
    customer.id,
    customer.company_name,
    customer.region,
    customer.credit_limit,
    customer.tags,
    customer.attributes
),
salesperson_rollup AS (
  SELECT
    facts.customer_id,
    salesperson.name AS leading_salesperson,
    department.name AS salesperson_department,
    count(*) AS handled_order_count,
    sum(facts.calculated_total) AS handled_revenue,
    row_number() OVER (
      PARTITION BY facts.customer_id
      ORDER BY
        sum(facts.calculated_total) DESC,
        count(*) DESC,
        salesperson.id
    ) AS salesperson_rank
  FROM order_facts AS facts
  JOIN lab.people AS salesperson
    ON salesperson.id = facts.salesperson_id
  LEFT JOIN lab.departments AS department
    ON department.id = salesperson.department_id
  GROUP BY
    facts.customer_id,
    salesperson.id,
    salesperson.name,
    department.name
),
audit_rollup AS (
  SELECT
    event.object_id::integer AS customer_id,
    count(*) AS audit_event_count,
    count(*) FILTER (
      WHERE event.event_type = 'viewed'
    ) AS viewed_event_count,
    count(*) FILTER (
      WHERE event.event_type = 'exported'
    ) AS exported_event_count,
    max(event.occurred_at) AS latest_audit_event_at,
    round(
      avg((event.payload ->> 'duration_ms')::numeric),
      2
    ) AS average_audit_duration_ms
  FROM lab.audit_events AS event
  CROSS JOIN parameters
  WHERE event.object_type = 'customer'
    AND event.object_id ~ '^[0-9]+$'
    AND event.occurred_at >= parameters.window_start
    AND event.occurred_at < parameters.window_end
  GROUP BY event.object_id
),
regional_ranked AS (
  SELECT
    customer.*,
    dense_rank() OVER (
      PARTITION BY customer.region
      ORDER BY customer.gross_revenue DESC
    ) AS revenue_rank_in_region,
    percent_rank() OVER (
      ORDER BY customer.gross_revenue
    ) AS revenue_percentile
  FROM customer_rollup AS customer
  CROSS JOIN parameters
  WHERE customer.order_count >= parameters.minimum_orders
),
final_report AS (
  SELECT
    ranked.customer_id,
    ranked.company_name,
    ranked.region,
    ranked.tags,
    ranked.newsletter_status,
    ranked.order_count,
    ranked.shipped_order_count,
    ranked.cancelled_order_count,
    ranked.salesperson_count,
    ranked.shipping_country_count,
    ranked.units,
    ranked.line_count,
    ranked.discounted_line_count,
    ranked.gross_revenue,
    ranked.average_order_value,
    ranked.average_discount_percent,
    ranked.credit_limit,
    round(ranked.gross_revenue / nullif(ranked.credit_limit, 0), 4)
      AS revenue_to_credit_ratio,
    ranked.first_order_at,
    ranked.latest_order_at,
    ranked.revenue_rank_in_region,
    round((ranked.revenue_percentile * 100)::numeric, 2)
      AS revenue_percentile,
    salesperson.leading_salesperson,
    salesperson.salesperson_department,
    salesperson.handled_order_count,
    salesperson.handled_revenue,
    coalesce(audit.audit_event_count, 0) AS audit_event_count,
    coalesce(audit.viewed_event_count, 0) AS viewed_event_count,
    coalesce(audit.exported_event_count, 0) AS exported_event_count,
    audit.latest_audit_event_at,
    audit.average_audit_duration_ms,
    CASE
      WHEN ranked.cancelled_order_count > ranked.shipped_order_count
        THEN 'review cancellations'
      WHEN ranked.average_discount_percent >= 8
        THEN 'review discounting'
      WHEN ranked.gross_revenue > ranked.credit_limit
        THEN 'consider credit increase'
      ELSE 'healthy'
    END AS account_signal
  FROM regional_ranked AS ranked
  LEFT JOIN salesperson_rollup AS salesperson
    ON salesperson.customer_id = ranked.customer_id
   AND salesperson.salesperson_rank = 1
  LEFT JOIN audit_rollup AS audit
    ON audit.customer_id = ranked.customer_id
)
SELECT
  report.*
FROM final_report AS report
CROSS JOIN parameters
ORDER BY
  report.gross_revenue DESC,
  report.company_name
LIMIT (SELECT result_limit FROM parameters);
