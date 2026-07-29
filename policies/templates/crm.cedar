permit (
  principal is SuperCampus::User,
  action in [SuperCampus::Action::"crm.leads.read", SuperCampus::Action::"crm.leads.update"],
  resource is SuperCampus::CrmLead
)
when { resource.tenantId == context.tenantId };